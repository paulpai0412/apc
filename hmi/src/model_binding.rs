//! Server-owned model roles: identifiers are opaque, never inferred from spelling.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{auth::Permission, opc_worker::NodesFile};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBinding {
    pub schema_version: u32,
    pub controller_name: String,
    pub sample_time: f64,
    pub prediction_horizon: usize,
    pub control_horizon: usize,
    pub cvs: Vec<BoundVariable<CvNodes>>,
    pub mvs: Vec<BoundVariable<MvNodes>>,
    pub dvs: Vec<BoundVariable<DvNodes>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoundVariable<N> {
    pub name: String,
    pub description: String,
    pub units: String,
    pub node_ids: N,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitNodes {
    pub low: String,
    pub high: String,
    #[serde(default, deserialize_with = "optional_id", skip_serializing_if = "Option::is_none")]
    pub ll: Option<String>,
    #[serde(default, deserialize_with = "optional_id", skip_serializing_if = "Option::is_none")]
    pub hh: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CvNodes {
    pub pv: String,
    pub target: String,
    pub prediction: String,
    pub limits: LimitNodes,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MvNodes {
    pub pv: String,
    pub sp: String,
    pub op: String,
    pub mode: String,
    pub mode_target: String,
    pub future_plan: String,
    #[serde(default, deserialize_with = "optional_id", skip_serializing_if = "Option::is_none")]
    pub applied_sp: Option<String>,
    #[serde(default, deserialize_with = "optional_id", skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub limits: LimitNodes,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DvNodes {
    pub pv: String,
    pub limits: LimitNodes,
}

fn optional_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    // Omission means unavailable feedback/role; an explicit null is malformed metadata.
    String::deserialize(deserializer).map(Some)
}

/// Every role is reserved, even when an optional diagnostic is not provisioned.
/// This prevents a configurable write ID from aliasing a producer-owned protocol ID.
#[derive(Default)]
pub struct RuntimeBindings {
    roles: HashMap<String, Option<Permission>>,
    provisioned: HashSet<String>,
}

impl RuntimeBindings {
    pub fn from_models(models: &[NodesFile]) -> Result<Self, String> {
        if models.is_empty() {
            return Err("No model bindings are available; runtime writes are disabled".into());
        }
        let mut bindings = Self::default();
        let mut controllers = HashSet::new();
        for model in models {
            let binding = model.model_binding.as_ref()
                .ok_or("Model binding is missing; regenerate the OPC node map before writing")?;
            valid_id(&binding.controller_name)?;
            if binding.schema_version != 1 || !binding.sample_time.is_finite() || binding.sample_time <= 0.0
                || binding.prediction_horizon == 0 || binding.control_horizon == 0
                || binding.control_horizon > binding.prediction_horizon
                || binding.cvs.is_empty() || binding.mvs.is_empty()
            {
                return Err(format!("Invalid model binding metadata for '{}'", binding.controller_name));
            }
            if !controllers.insert(binding.controller_name.as_str()) {
                return Err(format!("Ambiguous controller binding '{}'", binding.controller_name));
            }
            let mut provisioned = HashSet::new();
            for node in &model.nodes {
                valid_id(&node.node_id)?;
                if !provisioned.insert(node.node_id.as_str()) || !bindings.provisioned.insert(node.node_id.clone()) {
                    return Err(format!("Duplicate provisioned node '{}'", node.node_id));
                }
            }
            bindings.protocol(&binding.controller_name, "OperatingMode", Some(Permission::RuntimeWriteBasic))?;
            bindings.protocol(&binding.controller_name, "SaveConfiguration", Some(Permission::ModelDeploy))?;
            for suffix in ["Heartbeat", "AcquisitionSequence", "ExecutionTimeMs", "NextRun", "SolverStatus",
                "ConfigurationStatus", "ObjectiveFunction", "ControlObservation"] {
                bindings.protocol(&binding.controller_name, suffix, None)?;
            }
            let mut names = HashSet::new();
            for cv in &binding.cvs {
                unique_variable(&mut names, &cv.name)?;
                let nodes = &cv.node_ids;
                bindings.configured(&provisioned, &nodes.pv, None)?;
                bindings.configured(&provisioned, &nodes.prediction, None)?;
                bindings.configured(&provisioned, &nodes.target, Some(Permission::RuntimeWriteBasic))?;
                bindings.limits(&provisioned, &nodes.limits)?;
                bindings.protocol(&cv.name, "Weight", Some(Permission::ModelDeploy))?;
                bindings.protocol(&cv.name, "Alpha", Some(Permission::ModelDeploy))?;
                bindings.protocol(&cv.name, "Bias", None)?;
                bindings.protocol(&cv.name, "SteadyState", None)?;
            }
            for mv in &binding.mvs {
                unique_variable(&mut names, &mv.name)?;
                let nodes = &mv.node_ids;
                for id in [&nodes.pv, &nodes.op, &nodes.mode, &nodes.future_plan] {
                    bindings.configured(&provisioned, id, None)?;
                }
                if let Some(id) = &nodes.applied_sp {
                    bindings.configured(&provisioned, id, None)?;
                }
                for id in [&nodes.sp, &nodes.mode_target] {
                    bindings.configured(&provisioned, id, Some(Permission::RuntimeWriteBasic))?;
                }
                if let Some(id) = &nodes.target {
                    bindings.configured(&provisioned, id, Some(Permission::RuntimeWriteBasic))?;
                }
                bindings.limits(&provisioned, &nodes.limits)?;
                bindings.protocol(&mv.name, "Weight", Some(Permission::ModelDeploy))?;
                bindings.protocol(&mv.name, "LastMove", None)?;
                bindings.protocol(&mv.name, "SteadyState", None)?;
            }
            for dv in &binding.dvs {
                unique_variable(&mut names, &dv.name)?;
                bindings.configured(&provisioned, &dv.node_ids.pv, None)?;
                bindings.limits(&provisioned, &dv.node_ids.limits)?;
            }
        }
        Ok(bindings)
    }

    pub fn permission(&self, node_id: &str) -> Result<Permission, String> {
        if !self.provisioned.contains(node_id) {
            return Err(format!("Node '{node_id}' is not provisioned by the configured model source"));
        }
        match self.roles.get(node_id) {
            Some(Some(permission)) => Ok(*permission),
            Some(None) => Err(format!("Node '{node_id}' is read-only in the configured model binding")),
            None => Err(format!("Node '{node_id}' has no configured runtime write role")),
        }
    }

    fn insert(&mut self, id: &str, permission: Option<Permission>) -> Result<(), String> {
        valid_id(id)?;
        if self.roles.insert(id.to_owned(), permission).is_some() {
            return Err(format!("Ambiguous model binding: node '{id}' has multiple roles"));
        }
        Ok(())
    }

    fn configured(&mut self, provisioned: &HashSet<&str>, id: &str, permission: Option<Permission>) -> Result<(), String> {
        if !provisioned.contains(id) {
            return Err(format!("Model binding references unprovisioned node '{id}'"));
        }
        self.insert(id, permission)
    }

    fn protocol(&mut self, name: &str, suffix: &str, permission: Option<Permission>) -> Result<(), String> {
        self.insert(&format!("{name}:{suffix}"), permission)
    }

    fn limits(&mut self, provisioned: &HashSet<&str>, limits: &LimitNodes) -> Result<(), String> {
        for id in [&limits.low, &limits.high] {
            self.configured(provisioned, id, Some(Permission::RuntimeWriteLimitsOperational))?;
        }
        for id in [&limits.ll, &limits.hh].into_iter().flatten() {
            self.configured(provisioned, id, Some(Permission::RuntimeWriteLimitsSafety))?;
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err("Model binding contains an empty, oversized or control-character identifier".into());
    }
    Ok(())
}

fn unique_variable<'a>(names: &mut HashSet<&'a str>, name: &'a str) -> Result<(), String> {
    valid_id(name)?;
    if !names.insert(name) {
        return Err(format!("Ambiguous variable name '{name}'"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use serde_json::json;

    fn fixture() -> NodesFile {
        serde_json::from_value(json!({
            "model_binding": {
                "schema_version": 1, "controller_name": "cement-q1", "sample_time": 2.5,
                "prediction_horizon": 12, "control_horizon": 3,
                "cvs": [{"name":"Quality", "description":"Quality", "units":"%", "node_ids": {
                    "pv":"q.read", "target":"q.request", "prediction":"q.forecast",
                    "limits":{"low":"q.min", "high":"q.max", "ll":"q.tripA", "hh":"q.tripB"}
                }}],
                "mvs": [{"name":"Feed", "description":"Feed", "units":"t/h", "node_ids": {
                    "pv":"f.read", "sp":"f.request", "op":"f.output", "mode":"f.actual",
                    "mode_target":"f.modeRequest", "future_plan":"f.forecast", "applied_sp":"f.applied",
                    "target":"f.economic", "limits":{"low":"f.min", "high":"f.max"}
                }}], "dvs": []
            },
            "nodes": [
                {"node_id":"q.read"}, {"node_id":"q.request"}, {"node_id":"q.forecast"},
                {"node_id":"q.min"}, {"node_id":"q.max"}, {"node_id":"q.tripA"}, {"node_id":"q.tripB"},
                {"node_id":"f.read"}, {"node_id":"f.request"}, {"node_id":"f.output"},
                {"node_id":"f.actual"}, {"node_id":"f.modeRequest"}, {"node_id":"f.forecast"},
                {"node_id":"f.applied"}, {"node_id":"f.economic"}, {"node_id":"f.min"}, {"node_id":"f.max"},
                {"node_id":"cement-q1:OperatingMode"}, {"node_id":"cement-q1:AcquisitionSequence"},
                {"node_id":"cement-q1:SaveConfiguration"}, {"node_id":"Quality:Weight"},
                {"node_id":"Quality:Alpha"}, {"node_id":"Feed:Weight"}, {"node_id":"unmapped:SP"}
            ]
        })).unwrap()
    }

    #[test]
    fn custom_identifiers_receive_semantic_permissions_and_preserve_discovery_metadata() {
        let file = fixture();
        let roles = RuntimeBindings::from_models(&[file]).unwrap();
        for id in ["q.request", "f.request", "f.modeRequest", "f.economic", "cement-q1:OperatingMode"] {
            assert_eq!(roles.permission(id).unwrap(), Permission::RuntimeWriteBasic);
            assert!(Role::Operator.allows(roles.permission(id).unwrap()));
            assert!(!Role::Viewer.allows(roles.permission(id).unwrap()));
        }
        assert_eq!(roles.permission("q.min").unwrap(), Permission::RuntimeWriteLimitsOperational);
        assert_eq!(roles.permission("q.tripA").unwrap(), Permission::RuntimeWriteLimitsSafety);
        assert!(!Role::Operator.allows(roles.permission("q.tripA").unwrap()));
        for id in ["Quality:Weight", "Quality:Alpha", "Feed:Weight", "cement-q1:SaveConfiguration"] {
            assert_eq!(roles.permission(id).unwrap(), Permission::ModelDeploy);
            assert!(!Role::Operator.allows(roles.permission(id).unwrap()));
            assert!(Role::Engineer.allows(roles.permission(id).unwrap()));
        }
        let yaml = serde_yaml::to_string(&fixture()).unwrap();
        let decoded: NodesFile = serde_yaml::from_str(&yaml).unwrap();
        let returned = serde_json::to_value(decoded).unwrap();
        assert_eq!(returned["model_binding"]["controller_name"], "cement-q1");
        assert_eq!(returned["model_binding"]["sample_time"], 2.5);
        assert_eq!(returned["model_binding"]["mvs"][0]["node_ids"]["mode_target"], "f.modeRequest");
    }

    #[test]
    fn measurements_actual_modes_predictions_and_sequence_are_read_only_even_for_admin() {
        let roles = RuntimeBindings::from_models(&[fixture()]).unwrap();
        for id in ["q.read", "q.forecast", "f.read", "f.output", "f.actual", "f.forecast", "f.applied",
            "cement-q1:AcquisitionSequence", "unmapped:SP", "unknown:LowLimit"] {
            assert!(roles.permission(id).is_err(), "{id} must not be writable");
        }
    }

    #[test]
    fn safety_or_diagnostic_aliases_never_downgrade_authority() {
        for alias in ["q.tripA", "f.actual", "cement-q1:AcquisitionSequence", "Quality:Weight"] {
            let mut file = fixture();
            file.model_binding.as_mut().unwrap().cvs[0].node_ids.target = alias.into();
            assert!(RuntimeBindings::from_models(&[file]).is_err(), "alias {alias} must fail closed");
        }
        assert!(RuntimeBindings::from_models(&[fixture(), fixture()]).is_err());
    }

    #[test]
    fn missing_invalid_or_unprovisioned_bindings_fail_closed() {
        assert!(RuntimeBindings::from_models(&[]).is_err());
        let mut file = fixture();
        file.model_binding = None;
        assert!(RuntimeBindings::from_models(&[file]).is_err());
        let mut file = fixture();
        file.nodes.retain(|node| node.node_id != "f.request");
        assert!(RuntimeBindings::from_models(&[file]).is_err());
        for value in [0.0, -1.0, f64::NAN] {
            let mut file = fixture();
            file.model_binding.as_mut().unwrap().sample_time = value;
            assert!(RuntimeBindings::from_models(&[file]).is_err());
        }
        let mut file = fixture();
        file.model_binding.as_mut().unwrap().mvs[0].node_ids.sp.clear();
        assert!(RuntimeBindings::from_models(&[file]).is_err());
        let mut file = fixture();
        file.model_binding.as_mut().unwrap().schema_version = 2;
        assert!(RuntimeBindings::from_models(&[file]).is_err());
        let mut file = fixture();
        file.model_binding.as_mut().unwrap().control_horizon = 13;
        assert!(RuntimeBindings::from_models(&[file]).is_err());
        let mut serialized = serde_json::to_value(fixture()).unwrap();
        serialized["model_binding"]["mvs"][0]["node_ids"].as_object_mut().unwrap().remove("mode_target");
        assert!(serde_json::from_value::<NodesFile>(serialized).is_err());
        let mut serialized = serde_json::to_value(fixture()).unwrap();
        serialized["model_binding"]["mvs"][0]["node_ids"]["applied_sp"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<NodesFile>(serialized).is_err());
    }

    #[test]
    fn optional_feedback_and_different_variable_dimensions_keep_independent_roles() {
        let mut file = fixture();
        let binding = file.model_binding.as_mut().unwrap();
        binding.mvs[0].node_ids.applied_sp = None;
        binding.mvs[0].node_ids.target = None;
        binding.dvs.push(BoundVariable {
            name: "Disturbance".into(), description: "Measured disturbance".into(), units: "kg".into(),
            node_ids: DvNodes { pv: "d.read".into(), limits: LimitNodes {
                low: "d.min".into(), high: "d.max".into(), ll: None, hh: None,
            }},
        });
        for id in ["d.read", "d.min", "d.max"] {
            file.nodes.push(crate::opc_worker::NodeSpec {
                node_id: id.into(), node_class: None, description: None,
            });
        }
        let roles = RuntimeBindings::from_models(&[file]).unwrap();
        assert!(roles.permission("d.read").is_err());
        assert_eq!(roles.permission("d.max").unwrap(), Permission::RuntimeWriteLimitsOperational);
        assert!(roles.permission("f.applied").is_err());
        assert!(roles.permission("f.economic").is_err());
        assert_eq!(roles.permission("f.request").unwrap(), Permission::RuntimeWriteBasic);
    }
}
