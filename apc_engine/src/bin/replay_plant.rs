//! Loopback-only OPC UA adapter for an ideal actuator twin, not ABB PID/DA emulation.
//! Opt-in `--acquisition-outage-secs` enables Unix SIGUSR1/SIGUSR2 fault injection:
//! pause/resume complete acquisition frames while the fixed physical clock keeps running.
//! A pause is bounded by a monotonic deadline; duplicate pauses cannot extend it.
//! Signals take effect between frames, never midway through an OPC publication.
//! Ending the finite replay with publication suppressed omits terminal release/ACK
//! publication and fails closed instead of making an unobserved release claim.
use anyhow::{anyhow, ensure, Context, Result};
use authrity_apc_engine::{config::UnifiedModel, twin::{History, StepResponseTwin},
    native_lifecycle::Commissioning, replay_authority::IdealAuthority, opc_interface};
use clap::Parser;
use opcua::{client::{Client, ClientBuilder, IdentityToken, Session}, crypto::{PrivateKey, SecurityPolicy, X509},
    types::{AttributeId, DataValue, DateTime, MessageSecurityMode, NodeId, NumericRange, ReadValueId, TimestampsToReturn, Variant, WriteValue}};
use std::{collections::{BTreeMap, HashSet}, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::time::{sleep_until, timeout, Instant};

#[derive(Parser)]
#[command(about = "Finite loopback-only OPC UA plant replay; requires an isolated server and ideal OP/SP units")]
struct Args {
    #[arg(long)] plant: String,
    #[arg(long)] history: String,
    /// Literal loopback IP and port, e.g. opc.tcp://127.0.0.1:4840/.
    #[arg(long)] opc: String,
    #[arg(long)] namespace_index: u16,
    /// Must match Server_NamespaceArray at namespace-index, preventing wrong-node writes.
    #[arg(long)] namespace_uri: String,
    #[arg(long)] pki: PathBuf,
    #[arg(long)] certificate: PathBuf,
    #[arg(long)] private_key: PathBuf,
    /// Acknowledge all nodes are simulation-only, not a tunnel/proxy to production.
    #[arg(long, required = true)] acknowledge_isolated_simulator: bool,
    /// Acknowledge PV/OP/SP use identical engineering units and ideal actuator tracking.
    #[arg(long, required = true)] acknowledge_ideal_actuator: bool,
    /// Same administrator profile as native authority, with explicit fixture modes and watchdog.
    #[arg(long)] commissioning_profile: PathBuf,
    /// Explicit synthetic regime labels and separate sampled-setting observation tags.
    #[arg(long)] regime_config: Option<PathBuf>,
    /// Opt in to Unix SIGUSR1 pause / SIGUSR2 resume of complete acquisition publication.
    /// Physics stays live; pauses auto-expire after this finite positive duration (<=30s).
    #[arg(long, value_parser = parse_acquisition_outage_duration)]
    acquisition_outage_secs: Option<Duration>,
}

fn parse_acquisition_outage_duration(value: &str) -> std::result::Result<Duration, String> {
    let seconds: f64 = value.parse().map_err(|_| "acquisition outage must be a number of seconds".to_string())?;
    if !seconds.is_finite() || seconds <= 0.0 || seconds > 30.0 {
        return Err("acquisition outage must be finite, positive and at most 30 seconds".to_string());
    }
    let duration = Duration::try_from_secs_f64(seconds)
        .map_err(|_| "acquisition outage cannot be represented as a duration".to_string())?;
    if duration.is_zero() {
        return Err("acquisition outage is too small for the monotonic clock".to_string());
    }
    if !cfg!(unix) {
        return Err("acquisition outage signal controls require Unix".to_string());
    }
    Ok(duration)
}

/// Only publication is gated. No physical time, input or authority state lives here.
#[cfg(any(unix, test))]
struct AcquisitionOutage {
    duration: Duration,
    deadline: Option<Instant>,
}

#[cfg(any(unix, test))]
impl AcquisitionOutage {
    fn new(duration: Duration) -> Self {
        Self { duration, deadline: None }
    }

    fn begin(&mut self, now: Instant) -> bool {
        if self.deadline.is_some() { return false; }
        self.deadline = Some(now + self.duration);
        true
    }

    fn resume(&mut self) -> bool {
        self.deadline.take().is_some()
    }

    fn expire(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            true
        } else {
            false
        }
    }

    fn publication_allowed(&self) -> bool {
        self.deadline.is_none()
    }
}

#[cfg(unix)]
struct AcquisitionFault {
    outage: AcquisitionOutage,
    pause: tokio::signal::unix::Signal,
    resume: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl AcquisitionFault {
    fn new(duration: Duration) -> Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            outage: AcquisitionOutage::new(duration),
            pause: signal(SignalKind::user_defined1()).context("Register acquisition-outage SIGUSR1")?,
            resume: signal(SignalKind::user_defined2()).context("Register acquisition-outage SIGUSR2")?,
        })
    }

    fn log_transition(&self, event: &str, start: Instant, next_sample: usize, now: Instant) {
        eprintln!("Acquisition publication outage {event}: next_sample={next_sample} elapsed_secs={:.6} bound_secs={:.6} utc={:?}",
            now.duration_since(start).as_secs_f64(), self.outage.duration.as_secs_f64(), DateTime::now());
    }

    fn expire(&mut self, start: Instant, next_sample: usize, now: Instant) {
        if self.outage.expire(now) {
            self.log_transition("expired", start, next_sample, now);
        }
    }

    /// Consume signals only between frames. An in-flight frame always completes
    /// (or hits the existing fail-closed OPC error path), never a partial pause.
    async fn wait_for_tick(&mut self, tick: Instant, start: Instant, next_sample: usize) -> Result<bool> {
        loop {
            let expiry = self.outage.deadline.unwrap_or(tick);
            tokio::select! {
                // A signal flood cannot defer the physical tick or outage expiry.
                biased;
                _ = sleep_until(tick) => {
                    self.expire(start, next_sample, Instant::now());
                    return Ok(self.outage.publication_allowed());
                }
                _ = sleep_until(expiry), if self.outage.deadline.is_some() => {
                    self.expire(start, next_sample, Instant::now());
                }
                signal = self.pause.recv() => {
                    ensure!(signal.is_some(), "Acquisition-outage SIGUSR1 stream closed");
                    let now = Instant::now();
                    if self.outage.begin(now) {
                        self.log_transition("began", start, next_sample, now);
                    }
                }
                signal = self.resume.recv() => {
                    ensure!(signal.is_some(), "Acquisition-outage SIGUSR2 stream closed");
                    let now = Instant::now();
                    if self.outage.resume() {
                        self.log_transition("ended by SIGUSR2", start, next_sample, now);
                    }
                }
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RegimeConfig {
    constant_regimes: BTreeMap<String, f64>,
    control_mode_tag: String,
    sampled_settings: BTreeMap<String, String>,
}

fn loopback_endpoint(endpoint: &str) -> Result<()> {
    let rest = endpoint.strip_prefix("opc.tcp://").context("Endpoint must use opc.tcp://")?;
    ensure!(!rest.contains(['?', '#', '@']), "Endpoint cannot contain credentials, query or fragment");
    let authority = rest.split('/').next().unwrap_or("");
    let addr: SocketAddr = authority.parse().context("Endpoint requires a literal loopback IP and explicit port; DNS names forbidden")?;
    ensure!(addr.ip().is_loopback() && addr.port() != 0, "Only loopback simulator endpoints are allowed");
    Ok(())
}

async fn read_values(session: &Session, nodes: &[NodeId]) -> Result<Vec<DataValue>> {
    let requests: Vec<_> = nodes.iter().map(|id| ReadValueId { node_id: id.clone(), attribute_id: AttributeId::Value as u32,
        index_range: NumericRange::None, data_encoding: Default::default() }).collect();
    let values = timeout(Duration::from_secs(10), session.read(&requests, TimestampsToReturn::Both, 0.0)).await?
        .map_err(|e| anyhow!("OPC read failed: {e}"))?;
    ensure!(values.len() == nodes.len(), "OPC read result count mismatch");
    for (node, value) in nodes.iter().zip(&values) {
        ensure!(value.status.as_ref().map_or(true, |s| s.is_good()), "OPC non-good read status for {node:?}");
    }
    Ok(values)
}

async fn read_numbers(session: &Session, nodes: &[NodeId]) -> Result<Vec<f64>> {
    read_values(session, nodes).await?.into_iter().map(|value| {
        let number = match value.value {
            Some(Variant::Double(x)) => x,
            Some(Variant::Float(x)) => x as f64,
            Some(Variant::Int32(x)) => x as f64,
            Some(Variant::UInt32(x)) => x as f64,
            _ => anyhow::bail!("Expected numeric OPC node"),
        };
        ensure!(number.is_finite(), "Nonfinite OPC value");
        Ok(number)
    }).collect()
}

async fn write_values(session: &Session, nodes: &[NodeId], values: &[Variant]) -> Result<()> {
    write_values_at(session, nodes, values, DateTime::now()).await
}

async fn write_values_at(session: &Session, nodes: &[NodeId], values: &[Variant], acquired_at: DateTime) -> Result<()> {
    ensure!(nodes.len() == values.len(), "Write dimensions mismatch");
    let requests: Vec<_> = nodes.iter().zip(values).map(|(id, value)| WriteValue { node_id: id.clone(),
        attribute_id: AttributeId::Value as u32, index_range: NumericRange::None, value: {
            let mut value = DataValue::new_now(value.clone());
            value.source_timestamp = Some(acquired_at.clone());
            value
        } }).collect();
    let results = timeout(Duration::from_secs(10), session.write(&requests)).await?
        .map_err(|e| anyhow!("OPC write failed: {e}"))?;
    ensure!(results.len() == nodes.len(), "OPC write result count mismatch");
    for (node, status) in nodes.iter().zip(results) { ensure!(status.is_good(), "OPC write rejected for {node:?}: {status}"); }
    Ok(())
}

/// The acquisition marker is owned by this isolated adapter, never by the controller.
/// A rejected/partial publication leaves it odd and terminates replay (fail closed).
async fn begin_acquisition(session: &Session, node: &NodeId, sequence: &mut u64) -> Result<()> {
    ensure!(*sequence < (1_u64 << 53) - 2, "Acquisition sequence exhausted; provision a new isolated session");
    *sequence += if *sequence % 2 == 0 { 1 } else { 2 };
    write_values(session, std::slice::from_ref(node), &[Variant::Double(*sequence as f64)]).await
}

async fn finish_acquisition(session: &Session, node: &NodeId, sequence: &mut u64) -> Result<()> {
    ensure!(*sequence % 2 == 1, "Acquisition must be open before completion");
    *sequence += 1;
    write_values(session, std::slice::from_ref(node), &[Variant::Double(*sequence as f64)]).await
}

async fn run(args: &Args, session: &Session, model: &UnifiedModel, history: &History, twin: &mut StepResponseTwin) -> Result<()> {
    let ns = args.namespace_index;
    let profile_document: serde_json::Value = serde_json::from_slice(&std::fs::read(&args.commissioning_profile)?)?;
    let profile: Commissioning = serde_json::from_value(profile_document.clone())?;
    let regimes = args.regime_config.as_ref().map(|path| -> Result<RegimeConfig> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }).transpose()?;
    if let Some(performance) = profile_document.get("performance").filter(|value| !value.is_null()) {
        let config = regimes.as_ref().context("Performance profile requires explicit regime-config")?;
        let expected: Vec<String> = serde_json::from_value(performance["regime_tags"].clone())?;
        let actual: HashSet<_> = config.constant_regimes.keys().map(String::as_str)
            .chain(std::iter::once(config.control_mode_tag.as_str())).collect();
        ensure!(actual.len() == expected.len() && expected.iter().all(|tag| actual.contains(tag.as_str()))
            && !config.control_mode_tag.is_empty() && !config.constant_regimes.contains_key(&config.control_mode_tag)
            && config.constant_regimes.values().all(|value| value.is_finite()),
            "Regime labels must exactly match explicit commissioned synthetic sources");
        let sampled: BTreeMap<String,String> = serde_json::from_value(performance["sampled_tags"].clone())?;
        ensure!(sampled == config.sampled_settings, "Sampled setting observer map differs from commissioned performance source");
    } else {
        ensure!(regimes.is_none(), "Regime sources require a commissioned performance contract");
    }
    profile.validate_model(model)?;
    ensure!(profile.endpoint == args.opc && profile.handoff.acquire_value != profile.handoff.acknowledgement_value
        && profile.handoff.watchdog_timeout_ms > 0 && profile.handoff.local_mode.is_finite(),
        "Invalid isolated authority contract");
    let remote_modes = profile.remote_modes();
    ensure!(remote_modes.iter().all(|v| *v != profile.handoff.local_mode), "Remote/local fixture modes must differ");
    let local_modes = vec![profile.handoff.local_mode; remote_modes.len()];
    let release_node = NodeId::new(ns, profile.handoff.release_tag.as_str());
    let watchdog_node = NodeId::new(ns, profile.handoff.watchdog_tag.as_str());
    let acknowledgement_node = NodeId::new(ns, profile.handoff.acknowledgement_tag.as_str());
    let mut authority = IdealAuthority::default();
    ensure!(ns > 0, "Namespace zero cannot contain simulator model nodes");
    let namespace = read_values(session, &[NodeId::new(0, 2255u32)]).await?;
    // NamespaceArray is a String[]; conversion uses the library's typed Variant conversion.
    let namespaces: Vec<String> = match &namespace[0].value {
        Some(Variant::Array(array)) => array.values.iter().map(|value| match value {
            Variant::String(s) => Ok(s.to_string()),
            _ => Err(anyhow!("NamespaceArray contains non-string value")),
        }).collect::<Result<_>>()?,
        _ => anyhow::bail!("Server NamespaceArray is not an array"),
    };
    ensure!(namespaces.get(ns as usize) == Some(&args.namespace_uri), "Namespace URI/index mismatch");
    let mode = NodeId::new(ns, format!("{}:OperatingMode", model.metadata.name));
    let acquisition_name = profile.acquisition_sequence_node.clone();
    let acquisition_node = NodeId::new(ns, acquisition_name.as_str());
    let initial_sequence = read_numbers(session, std::slice::from_ref(&acquisition_node)).await?[0];
    ensure!(initial_sequence >= 0.0 && initial_sequence.fract() == 0.0 && initial_sequence < ((1_u64 << 53) - 2) as f64,
        "AcquisitionSequence must be an exact nonnegative integer below 2^53-2");
    let mut sequence = initial_sequence as u64;
    let initial_mode = read_numbers(session, &[mode.clone()]).await?[0];
    ensure!(initial_mode == 0.0 || initial_mode == 1.0, "Controller must be Off/Monitor before baseline initialization");
    let mut names = HashSet::new();
    let mut nodes = Vec::new();
    let mut initial = Vec::new();
    let mut live_nodes = Vec::new();
    let mut live_values = Vec::new();
    let mut sp_nodes = Vec::new();
    let mut actual_mode_nodes = Vec::new();
    let mut mode_target_nodes = Vec::new();
    // Reject aliases, including any accidental alias to the controller arming node.
    names.insert(format!("{}:OperatingMode", model.metadata.name));
    names.insert(acquisition_name.clone());
    for tag in [&profile.handoff.release_tag, &profile.handoff.watchdog_tag, &profile.handoff.acknowledgement_tag] {
        ensure!(names.insert(tag.clone()), "Fixture authority tag alias");
    }
    for mv in &model.variables.mvs {
        for name in [&mv.node_ids.mode, &mv.node_ids.mode_target] {
            ensure!(!name.trim().is_empty() && !name.starts_with("ns=") && names.insert(name.clone()),
                "Operator-owned PID mode nodes must be distinct namespace-relative identifiers");
        }
    }
    let mut add = |name: &str, value: Variant, live: bool| -> Result<usize> {
        ensure!(!name.trim().is_empty() && !name.starts_with("ns=") && names.insert(name.to_string()), "Node IDs must be distinct, nonempty namespace-relative string identifiers: {name}");
        let node = NodeId::new(ns, name);
        nodes.push(node.clone());
        initial.push(value.clone());
        if live { live_nodes.push(node); live_values.push(value); }
        Ok(nodes.len() - 1)
    };
    for (i, cv) in model.variables.cvs.iter().enumerate() { add(&cv.node_ids.pv, history.cv[0][i].into(), true)?; }
    for (i, dv) in model.variables.dvs.iter().enumerate() { add(&dv.node_ids.pv, history.dv[0][i].into(), true)?; }
    for (j, mv) in model.variables.mvs.iter().enumerate() {
        ensure!(history.mv[0][j] >= mv.limits.low && history.mv[0][j] <= mv.limits.high, "Initial MV outside plant limits");
        add(&mv.node_ids.pv, history.mv[0][j].into(), true)?;
        add(&mv.node_ids.op, history.mv[0][j].into(), true)?;
        add(&mv.node_ids.sp, history.mv[0][j].into(), false)?;
        if let Some(applied_sp) = mv.node_ids.applied_sp.as_deref() {
            add(applied_sp, history.mv[0][j].into(), true)?;
        }
        // The fixture owns ACTUAL modes; only native authority writes mode requests.
        actual_mode_nodes.push(NodeId::new(ns, mv.node_ids.mode.as_str()));
        mode_target_nodes.push(NodeId::new(ns, mv.node_ids.mode_target.as_str()));
        sp_nodes.push(NodeId::new(ns, mv.node_ids.sp.as_str()));
    }
    // Probe all node existence/status before the first write; failed writes stop immediately.
    read_values(session, &nodes).await?;
    ensure!(read_numbers(session, std::slice::from_ref(&release_node)).await?[0] == profile.handoff.release_value,
        "Initialize explicit fixture release before starting the physical producer");
    read_values(session, &mode_target_nodes).await?;
    let settings_nodes: Vec<NodeId> = profile.variables.iter().filter(|v| v.role != "DV").flat_map(|v|
        [&v.target_tag, &v.low_tag, &v.high_tag].into_iter().flatten())
        .map(|tag| NodeId::new(ns, tag.as_str())).collect();
    let initial_settings = read_numbers(session, &settings_nodes).await?;
    let physical_values = live_values.len();
    if let Some(config) = &regimes {
        let setting_tags: Vec<_> = profile.variables.iter().filter(|v| v.role != "DV")
            .flat_map(|v| [&v.target_tag, &v.low_tag, &v.high_tag].into_iter().flatten()).collect();
        ensure!(setting_tags.len() == config.sampled_settings.len(), "Every sampled operator setting needs exactly one independent observer");
        for (tag, value) in setting_tags.iter().zip(&initial_settings) {
            let observer = config.sampled_settings.get(*tag).context("Missing sampled-setting observer")?;
            ensure!(names.insert(observer.clone()) && !setting_tags.contains(&observer), "Sampled observer aliases an owned/control tag");
            live_nodes.push(NodeId::new(ns, observer.as_str())); live_values.push(Variant::Double(*value));
        }
        for (tag, value) in &config.constant_regimes {
            ensure!(names.insert(tag.clone()), "Regime tag aliases a physical/control tag");
            live_nodes.push(NodeId::new(ns, tag.as_str())); live_values.push(Variant::Double(*value));
        }
        ensure!(names.insert(config.control_mode_tag.clone()), "Control-mode regime aliases another tag");
        live_nodes.push(NodeId::new(ns, config.control_mode_tag.as_str()));
        live_values.push(Variant::Double(profile.handoff.local_mode));
        read_values(session, &live_nodes).await?;
    }
    // Register only on explicit opt-in, before the initialized/ready publication.
    // A signal received during initialization is consumed at the first frame boundary.
    #[cfg(unix)]
    let mut acquisition_fault = args.acquisition_outage_secs.map(AcquisitionFault::new).transpose()?;
    begin_acquisition(session, &acquisition_node, &mut sequence).await?;
    write_values(session, &nodes, &initial).await?;
    opc_interface::write_numeric_bulk(session, &actual_mode_nodes, &local_modes).await?;
    opc_interface::write_numeric_bulk(session, std::slice::from_ref(&acknowledgement_node),
        &[profile.handoff.acknowledgement_value]).await?;
    finish_acquisition(session, &acquisition_node, &mut sequence).await?;
    eprintln!("Initialized released isolated ideal actuator; physical time advances continuously in Monitor. Native explicit arm/Engage required.");
    let dt = Duration::try_from_secs_f64(history.sample_time).context("sample_time cannot be represented as a wall-clock duration")?;
    ensure!(!dt.is_zero(), "sample_time too small for wall clock");
    let start = Instant::now();
    let mut previous = history.mv[0].clone();
    for t in 1..history.cv.len() {
        let elapsed = Duration::try_from_secs_f64(t as f64 * history.sample_time).context("Replay duration overflow")?;
        let deadline = start.checked_add(elapsed).context("Replay clock overflow")?;
        ensure!(Instant::now() < deadline, "OPC processing exceeded sample_time; refusing accelerated catch-up");
        #[cfg(unix)]
        let publish = if let Some(fault) = acquisition_fault.as_mut() {
            fault.wait_for_tick(deadline, start, t).await?
        } else {
            sleep_until(deadline).await;
            true
        };
        #[cfg(not(unix))]
        let publish = {
            sleep_until(deadline).await;
            true
        };
        // One decision covers the marker and every member of this complete frame.
        // No signal handling inside the frame can split its publication.
        if publish { begin_acquisition(session, &acquisition_node, &mut sequence).await?; }
        let acquired_at = DateTime::now();
        let authority_values = read_numbers(session, &[release_node.clone(), watchdog_node.clone()]).await?;
        let targets = read_numbers(session, &mode_target_nodes).await?;
        let active = authority.observe(&profile.handoff, authority_values[0], authority_values[1],
            &targets, &remote_modes, std::time::Instant::now())?;
        // Settings are acquired from their real operator-owned registers. Reading
        // them does not fabricate a changed value or refresh their source timestamps.
        let sampled_settings = timeout(dt / 4, async {
            let mut snapshot = read_numbers(session, &settings_nodes).await?;
            loop {
                let next = read_numbers(session, &settings_nodes).await?;
                if next == snapshot { break Ok::<_, anyhow::Error>(next); }
                snapshot = next;
            }
        }).await.context("Operator configuration did not stabilize within the physical acquisition budget")??;
        let command = if active { read_numbers(session, &sp_nodes).await? } else { previous.clone() };
        for (j, mv) in model.variables.mvs.iter().enumerate() {
            ensure!(command[j] >= mv.limits.low_low && command[j] <= mv.limits.high_high
                && (command[j] - previous[j]).abs() <= mv.max_move + 1e-7 * mv.max_move.max(1.0),
                "MV command violates physical hard bounds/rate; acquisition remains invalid");
        }
        let cv = twin.advance(&command, &history.dv[t])?;
        let mut index = 0;
        for value in cv.iter().chain(&history.dv[t]) { live_values[index] = Variant::Double(*value); index += 1; }
        for (j, value) in command.iter().enumerate() {
            // This adapter is an explicitly acknowledged ideal-actuator twin: PV/OP
            // are ideal telemetry proxies. AppliedSP is the ACCEPTED model input,
            // never a delayed downstream actuator realization (no double actuator lag).
            live_values[index] = Variant::Double(*value);
            live_values[index + 1] = Variant::Double(*value);
            index += 2;
            if model.variables.mvs[j].node_ids.applied_sp.is_some() {
                live_values[index] = Variant::Double(*value);
                index += 1;
            }
        }
        if regimes.is_some() {
            for (index, value) in sampled_settings.iter().enumerate() {
                live_values[physical_values + index] = Variant::Double(*value);
            }
            // The single mode classification is only valid if all ideal loops share
            // that actual mode. Mixed modes are rejected rather than averaged.
            ensure!(!active || remote_modes.iter().all(|mode| *mode == remote_modes[0]), "Mixed PID modes cannot form one regime label");
            *live_values.last_mut().context("Missing regime mode slot")? = Variant::Double(
                if active { remote_modes[0] } else { profile.handoff.local_mode });
        }
        if publish {
            write_values_at(session, &live_nodes, &live_values, acquired_at.clone()).await?;
            opc_interface::write_numeric_bulk_at(session, &actual_mode_nodes, if active { &remote_modes } else { &local_modes }, acquired_at.clone()).await?;
            // Independent ACK follows the actual local-mode/applied-coordinate snapshot.
            opc_interface::write_numeric_bulk_at(session, std::slice::from_ref(&acknowledgement_node),
                &[if active { profile.handoff.acquire_value } else { profile.handoff.acknowledgement_value }], acquired_at).await?;
            finish_acquisition(session, &acquisition_node, &mut sequence).await?;
        }
        previous.copy_from_slice(&command);
    }
    // Termination uses the last physical frame's publication decision. Do not
    // synthesize a release/ACK or keep publishing a frozen final sample to wait out
    // an outage: consumers must see expiry, and shutdown must report failure.
    #[cfg(unix)]
    if acquisition_fault.as_ref().is_some_and(|fault| !fault.outage.publication_allowed()) {
        anyhow::bail!("Finite physical replay ended with acquisition publication suppressed: last_sample={} elapsed_secs={:.6}; terminal release/ACK NOT published; acquisition will expire; no restart/rearm is implied",
            history.cv.len() - 1, start.elapsed().as_secs_f64());
    }
    begin_acquisition(session, &acquisition_node, &mut sequence).await?;
    opc_interface::write_numeric_bulk(session, &actual_mode_nodes, &local_modes).await?;
    opc_interface::write_numeric_bulk(session, std::slice::from_ref(&acknowledgement_node),
        &[profile.handoff.acknowledgement_value]).await?;
    finish_acquisition(session, &acquisition_node, &mut sequence).await?;
    eprintln!("Finite physical replay complete; ideal actuator released at last accepted SP. Acquisition will now expire; no restart/rearm is implied.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    ensure!(args.acknowledge_isolated_simulator && args.acknowledge_ideal_actuator, "Both isolation and ideal-actuator acknowledgements are required");
    loopback_endpoint(&args.opc)?;
    let model = UnifiedModel::load(&args.plant)?;
    let history = History::load(&args.history)?;
    history.validate(&model)?;
    let mut twin = StepResponseTwin::new(&model, &history.cv[0], &history.mv[0], &history.dv[0])?;
    for mv in &model.variables.mvs {
        ensure!([mv.limits.low, mv.limits.high, mv.max_move].iter().all(|x| x.is_finite())
            && mv.limits.low < mv.limits.high && mv.max_move > 0.0,
            "Invalid simulated actuator bounds/rate for {}", mv.name);
    }
    let duration = Duration::try_from_secs_f64((history.cv.len() - 1) as f64 * history.sample_time)
        .context("History duration cannot be represented by wall clock")?;
    ensure!(Instant::now().checked_add(duration).is_some(), "History duration exceeds wall clock");
    ensure!(!Duration::try_from_secs_f64(history.sample_time)?.is_zero(), "sample_time too small for wall clock");
    let cert = X509::from_der(&std::fs::read(&args.certificate)?).map_err(|e| anyhow!("Invalid certificate: {e:?}"))?;
    let key = PrivateKey::from_pem(&std::fs::read(&args.private_key)?).map_err(|e| anyhow!("Invalid private key: {e:?}"))?;
    let mut client = ClientBuilder::new().application_name("AuTHRity Isolated Replay Plant")
        .application_uri("urn:authrity:isolated-replay-plant").pki_dir(args.pki.clone())
        .certificate_path(&args.certificate).private_key_path(&args.private_key)
        .create_sample_keypair(false).trust_server_certs(false).session_retry_limit(0)
        .client().map_err(|e| anyhow!("OPC client configuration: {e:?}"))?;
    // Discover on loopback, then pin the advertised endpoint to the exact supplied URL.
    // Do not allow discovery to redirect this simulator to another host.
    let endpoints = timeout(Duration::from_secs(10), client.get_server_endpoints_from_url(args.opc.as_str())).await
        .context("OPC endpoint discovery timed out")?
        .map_err(|e| anyhow!("OPC discovery: {e}"))?;
    let endpoint = Client::find_matching_endpoint(&endpoints, &args.opc, SecurityPolicy::Basic256Sha256, MessageSecurityMode::SignAndEncrypt)
        .context("Isolated server must expose Basic256Sha256/SignAndEncrypt")?;
    ensure!(endpoint.endpoint_url.to_string() == args.opc, "Server advertised a different endpoint URL; refusing redirection");
    let (session, event_loop) = client.connect_to_endpoint_directly(endpoint,
        IdentityToken::X509(Box::new(cert), Box::new(key))).map_err(|e| anyhow!("OPC connection: {e}"))?;
    let handle = event_loop.spawn();
    ensure!(timeout(Duration::from_secs(10), session.wait_for_connection()).await
        .context("OPC secure session connection timed out")?,
        "OPC session did not become connected");
    let result = run(&args, &session, &model, &history, &mut twin).await;
    handle.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(extra: &[&str]) -> std::result::Result<Args, clap::Error> {
        Args::try_parse_from([
            "replay_plant", "--plant", "plant.json", "--history", "history.csv",
            "--opc", "opc.tcp://127.0.0.1:4840/", "--namespace-index", "2",
            "--namespace-uri", "urn:isolated:test", "--pki", "pki",
            "--certificate", "client.der", "--private-key", "client.pem",
            "--acknowledge-isolated-simulator", "--acknowledge-ideal-actuator",
            "--commissioning-profile", "commissioning.json",
        ].into_iter().chain(extra.iter().copied()))
    }

    #[test]
    fn acquisition_outage_requires_explicit_cli_opt_in() {
        assert!(parse_args(&[]).unwrap().acquisition_outage_secs.is_none());
        assert!(parse_args(&["--acquisition-outage-secs"]).is_err());
        #[cfg(unix)]
        assert_eq!(parse_args(&["--acquisition-outage-secs", "12"]).unwrap()
            .acquisition_outage_secs, Some(Duration::from_secs(12)));
        #[cfg(not(unix))]
        assert!(parse_args(&["--acquisition-outage-secs", "12"]).err().unwrap()
            .to_string().contains("Unix"));
    }

    #[test]
    fn acquisition_outage_rejects_unbounded_or_unrepresentable_cli_durations() {
        for value in ["0", "-1", "NaN", "inf", "-inf", "30.000001", "1e300", "1e-12", "invalid"] {
            let option = format!("--acquisition-outage-secs={value}");
            assert!(parse_args(&[&option]).is_err(), "accepted {value}");
        }
        #[cfg(unix)]
        for (value, expected) in [("0.001", Duration::from_millis(1)), ("30", Duration::from_secs(30))] {
            assert_eq!(parse_args(&["--acquisition-outage-secs", value]).unwrap()
                .acquisition_outage_secs, Some(expected));
        }
    }

    #[test]
    fn duplicate_pause_does_not_extend_consumer_publication_outage() {
        let start = Instant::now();
        let mut outage = AcquisitionOutage::new(Duration::from_secs(12));
        assert!(outage.publication_allowed());
        assert!(outage.begin(start));
        assert!(!outage.publication_allowed());
        assert!(!outage.begin(start + Duration::from_secs(11)));
        assert!(!outage.expire(start + Duration::from_millis(11_999)));
        assert!(!outage.publication_allowed());
        assert!(outage.expire(start + Duration::from_secs(12)));
        assert!(outage.publication_allowed());
        assert!(!outage.expire(start + Duration::from_secs(23)));
    }

    #[test]
    fn explicit_resume_reopens_publication_without_waiting_for_expiry() {
        let start = Instant::now();
        let mut outage = AcquisitionOutage::new(Duration::from_secs(12));
        assert!(!outage.resume());
        assert!(outage.begin(start));
        assert!(!outage.expire(start + Duration::from_secs(1)));
        assert!(outage.resume());
        assert!(outage.publication_allowed());
        assert!(!outage.resume());
        assert!(!outage.expire(start + Duration::from_secs(12)));
        assert!(outage.begin(start + Duration::from_secs(13)));
        assert!(!outage.publication_allowed());
        assert!(outage.expire(start + Duration::from_secs(25)));
        assert!(outage.publication_allowed());
    }
}

