//! History selection is derived only from the commissioned profile, never browser-supplied tags.
use super::*;
use axum::extract::Query;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {profile_hash:String,start:DateTime<Utc>,end:DateTime<Utc>,max_points:Option<usize>}

pub(super) fn routes()->Router<AppState>{Router::new().route("/api/runtime/trends",get(history))}

async fn history(State(state):State<AppState>,user:CurrentUser,Query(query):Query<HistoryQuery>)->Result<Json<Value>> {
    if state.settings.research.enabled{return Err(invalid("Isolated research exposes commissioning metadata, not field historian connections"));}
    let profile=state.runtime.profile(&query.profile_hash,&user,"runtime.view")?;
    if query.start>=query.end||query.end>Utc::now()||query.end-query.start>chrono::Duration::days(31) {
        return Err(invalid("Select an ordered past history window of at most 31 days"));
    }
    let mut nodes=BTreeSet::new();let mut settings=BTreeSet::new();
    for variable in &profile.variables {
        nodes.insert(variable.pv_tag.clone());
        for tag in [&variable.command_tag,&variable.applied_tag,&variable.op_tag].into_iter().flatten(){nodes.insert(tag.clone());}
        if let Some(tag)=&variable.command_tag{settings.insert(tag.clone());}
        for original in [&variable.target_tag,&variable.low_tag,&variable.high_tag].into_iter().flatten() {
            let tag=profile.performance.as_ref().and_then(|contract|contract.sampled_tags.get(original)).unwrap_or(original);
            nodes.insert(tag.clone());settings.insert(tag.clone());
        }
    }
    let nodes:Vec<_>=nodes.into_iter().collect();let settings:Vec<_>=settings.into_iter().collect();
    let history=crate::process_trends::query_scoped(&state.historian_config,query.start,query.end,&nodes,&settings,query.max_points,&profile.endpoint)
        .await.map_err(|error|invalid(error.to_string()))?;
    let mut response=serde_json::to_value(history).map_err(storage)?;
    response["profile_hash"]=json!(profile.profile_hash);
    response["sampled_tags"]=json!(profile.performance.as_ref().map(|contract|&contract.sampled_tags));
    response["window"]=json!({"start":query.start,"end":query.end});
    response["reduction"]=json!(if query.max_points.is_some(){"extrema_preserving"}else{"full_resolution"});
    response["historical_forecast_available"]=json!(false);
    response["forecast_unavailable_reason"]=json!("Historian numeric rows do not preserve attested forecast issue-time, version and instance identity. Historical forecasts are unavailable; live native forecasts are separate.");
    Ok(Json(response))
}
