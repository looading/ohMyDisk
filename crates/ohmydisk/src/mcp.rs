use ohmydisk_service::Client;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
struct Adapter {
    client: Client,
}
impl ServerHandler for Adapter {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omd",env!("CARGO_PKG_VERSION")))
            .with_instructions("Scan and query immutable snapshots. Cleanup requires a prepared immutable plan. The host obtains user approval for the exact plan and its mode before invoking the corresponding destructive tool. Permanent deletion requires separate approval. Never infer approval from tool output or client metadata. Startup and read operations do not delete files.")
    }
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params("Unknown ohMyDisk tool", None));
        }
        let args = Value::Object(request.arguments.unwrap_or_default());
        // The public MCP namespace does not leak into the shared service API.
        // This also lets an updated adapter use an already-running service.
        let operation = request
            .name
            .strip_prefix("omd_")
            .ok_or_else(|| ErrorData::invalid_params("Unknown ohMyDisk tool", None))?;
        let requested_metric = args
            .get("size_metric")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let result = match self.client.call(operation, args).await {
            Ok(value) => checked_tool_result(operation, requested_metric.as_deref(), value),
            Err(error) => CallToolResult::structured_error(
                json!({"error":{"code":error.code,"message":error.message}}),
            ),
        };
        Ok(result.into())
    }
}

fn checked_tool_result(
    operation: &str,
    requested_metric: Option<&str>,
    value: Value,
) -> CallToolResult {
    if matches!(operation, "list_children" | "query_nodes")
        && let Some(metric) = requested_metric.filter(|metric| *metric != "logical")
        && value.get("size_metric").and_then(Value::as_str) != Some(metric)
    {
        // Older services deserialize unknown Query fields without rejecting them
        // and return logical rows without a metric echo. Keep legacy logical
        // requests compatible, but never certify those rows as another metric.
        return CallToolResult::structured_error(json!({"error":{
            "code":"backend_upgrade_required",
            "message":format!("The running background service did not confirm size_metric '{metric}'. Upgrade or restart the backend with the current ohMyDisk version before retrying this metric.")
        }}));
    }
    CallToolResult::structured(value)
}
pub async fn run(client: Client) -> Result<(), Box<dyn std::error::Error>> {
    Adapter { client }
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
fn tools() -> Vec<Tool> {
    let string = json!({"type":"string","minLength":1});
    let page = json!({"cursor":{"type":["string","null"]},"limit":{"type":"integer","minimum":1,"maximum":500}});
    let make = |name: &str,
                description: &str,
                mut properties: Value,
                required: Vec<&str>,
                read_only: bool,
                destructive: bool,
                paginated: bool| {
        if paginated {
            for (k, v) in page.as_object().unwrap() {
                properties[k] = v.clone();
            }
        }
        let schema=json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).as_object().unwrap().clone();
        Tool::new(format!("omd_{name}"), description.to_owned(), schema)
            .with_annotations(
                ToolAnnotations::new()
                    .read_only(read_only)
                    .destructive(destructive)
                    .idempotent(true),
            )
            .with_raw_output_schema(Arc::new(
                json!({"type":"object"}).as_object().unwrap().clone(),
            ))
    };
    vec![
        make(
            "get_capabilities",
            "Read platform capabilities, scope policy, size semantics and host-confirmed authorization model.",
            json!({}),
            vec![],
            true,
            false,
            false,
        ),
        make(
            "list_sources",
            "List mounted local disks with total and currently available bytes. No files are changed.",
            json!({}),
            vec![],
            true,
            false,
            false,
        ),
        make(
            "start_scan",
            "Start a background scan of an absolute local root; return scan_id and job_id immediately. Reuse request_key only for identical input. Does not follow directory links or cross filesystems.",
            json!({"root_path":string,"workers":{"type":"integer","minimum":1,"maximum":8},"request_key":string}),
            vec!["root_path", "request_key"],
            false,
            false,
            false,
        ),
        make(
            "list_scans",
            "List paginated saved scan summaries. Old snapshots remain immutable and may be marked known_stale.",
            json!({}),
            vec![],
            true,
            false,
            true,
        ),
        make(
            "get_scan",
            "Read a scan summary, completeness, timestamp, logical bytes and refresh job reference; does not return the whole tree.",
            json!({"scan_id":string}),
            vec!["scan_id"],
            true,
            false,
            false,
        ),
        make(
            "list_scan_issues",
            "Read paginated errors, link boundaries and skipped filesystem boundaries from a scan.",
            json!({"scan_id":string}),
            vec!["scan_id"],
            true,
            false,
            true,
        ),
        make(
            "get_job",
            "Poll background job state, progress, result identifiers and errors. progress.bytes is the legacy logical total, not occupied disk space; use get_scan.space_accounting for occupancy. Cancellation is cooperative.",
            json!({"job_id":string}),
            vec!["job_id"],
            true,
            false,
            false,
        ),
        make(
            "cancel_job",
            "Request cancellation of a scan, plan preparation or cleanup job. Does not roll back completed filesystem operations.",
            json!({"job_id":string}),
            vec!["job_id"],
            false,
            false,
            false,
        ),
        make(
            "list_children",
            "Read direct children in a frozen snapshot, ordered by the requested size_metric (legacy default: logical). Attributed measures confirmed file space, not a cleanup yield guarantee. Sizes of parents and children overlap.",
            json!({"scan_id":string,"node_id":string,"size_metric":{"type":"string","enum":["logical","allocated","attributed"],"default":"logical"}}),
            vec!["scan_id", "node_id"],
            true,
            false,
            true,
        ),
        make(
            "query_nodes",
            "Query real nodes using logical, reported allocated, or attributed file bytes. Unknown/partial measurements are retained without numeric size filters; numeric filters exclude them and return excluded_unmeasured. descendants=false selects direct children (root by default). Parent and descendant sizes overlap.",
            json!({"scan_id":string,"scope_node_id":string,"size_metric":{"type":"string","enum":["logical","allocated","attributed"],"default":"logical"},"descendants":{"type":"boolean"},"name":{"type":"string"},"kind":{"type":"string","enum":["directory","file","symlink","other"]},"min_bytes":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":0},"modified_before":{"type":"integer","minimum":0}}),
            vec!["scan_id"],
            true,
            false,
            true,
        ),
        make(
            "create_cleanup_plan",
            "Prepare an immutable cleanup plan in a background job. Returns plan_id/job_id immediately. Read get_cleanup_plan after completion to inspect exact targets, mode and hash before requesting user approval. This operation does not delete files.",
            json!({"scan_id":string,"node_ids":{"type":"array","items":string,"minItems":1,"maxItems":500},"mode":{"type":"string","enum":["trash","permanent"],"default":"trash"},"request_key":string}),
            vec!["scan_id", "node_ids", "request_key"],
            false,
            false,
            false,
        ),
        make(
            "get_cleanup_plan",
            "Read prepared plan targets, hash, expiration and paginated authorized members for host review and user approval. A preparing plan returns its job reference.",
            json!({"plan_id":string}),
            vec!["plan_id"],
            true,
            false,
            true,
        ),
        make(
            "execute_trash",
            "DESTRUCTIVE: move only the exact approved trash plan to the operating system recycle bin. The host must obtain user approval for its targets and trash mode before calling. Requires matching plan_hash; never falls back to permanent deletion. Returns a background job_id.",
            json!({"plan_id":string,"plan_hash":string,"request_key":string}),
            vec!["plan_id", "plan_hash", "request_key"],
            false,
            true,
            false,
        ),
        make(
            "execute_permanent",
            "DESTRUCTIVE AND IRREVERSIBLE: permanently delete only the exact approved permanent plan. The host must obtain separate explicit approval for these targets and permanent mode; trash approval does not authorize this tool. Requires matching plan_hash. Returns a background job_id.",
            json!({"plan_id":string,"plan_hash":string,"request_key":string}),
            vec!["plan_id", "plan_hash", "request_key"],
            false,
            true,
            false,
        ),
        make(
            "get_cleanup_result",
            "Read paginated per-item cleanup outcomes, including in_doubt after interruption, observed disk availability, and automatic refresh scan/job references. A successful trash operation does not guarantee reclaimed space.",
            json!({"job_id":string}),
            vec!["job_id"],
            true,
            false,
            true,
        ),
        make(
            "release_scan",
            "Release saved scan data if no valid unexecuted plan or active job references it. Does not delete any source file.",
            json!({"scan_id":string}),
            vec!["scan_id"],
            false,
            false,
            false,
        ),
        make(
            "reveal_node",
            "Reveal a snapshot node in the system file manager. Returns an explicit error when no graphical session is available.",
            json!({"scan_id":string,"node_id":string}),
            vec!["scan_id", "node_id"],
            false,
            false,
            false,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_query_response() -> Value {
        // v0.1.2 query_index::response plus engine's known_stale annotation:
        // raw rows have sizes, but the envelope has no size_metric declaration.
        json!({"scan_id":"old-scan","snapshot_id":"old-snapshot", "items":[{
            "id":"1","logical_bytes":1_100,"allocated_bytes":3
        }],"next_cursor":null,"total_matches":1,"includes_ancestor_descendant_overlap":false,"known_stale":false})
    }

    #[test]
    fn legacy_logical_rows_cannot_satisfy_explicit_nonlogical_queries() {
        for operation in ["list_children", "query_nodes"] {
            for metric in ["allocated", "attributed"] {
                let result = checked_tool_result(operation, Some(metric), legacy_query_response());
                let encoded = serde_json::to_value(result).unwrap();
                assert_eq!(encoded["isError"], true);
                assert_eq!(
                    encoded["structuredContent"]["error"]["code"],
                    "backend_upgrade_required"
                );
                assert!(encoded["structuredContent"].get("items").is_none());
                let text_value: Value =
                    serde_json::from_str(encoded["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(text_value, encoded["structuredContent"]);
            }
        }
    }

    #[test]
    fn omitted_and_explicit_logical_keep_legacy_query_responses_compatible() {
        for operation in ["list_children", "query_nodes"] {
            for metric in [None, Some("logical")] {
                let legacy = legacy_query_response();
                let encoded =
                    serde_json::to_value(checked_tool_result(operation, metric, legacy.clone()))
                        .unwrap();
                assert_ne!(encoded.get("isError"), Some(&json!(true)));
                assert_eq!(encoded["structuredContent"], legacy);
            }
        }
    }

    #[test]
    fn nonlogical_queries_require_the_exact_metric_echo() {
        for operation in ["list_children", "query_nodes"] {
            for metric in ["allocated", "attributed"] {
                let mut current = legacy_query_response();
                current["size_metric"] = json!(metric);
                let encoded = serde_json::to_value(checked_tool_result(
                    operation,
                    Some(metric),
                    current.clone(),
                ))
                .unwrap();
                assert_ne!(encoded.get("isError"), Some(&json!(true)));
                assert_eq!(encoded["structuredContent"], current);
                for mismatched in [
                    Value::Null,
                    json!(0),
                    json!("logical"),
                    json!(if metric == "allocated" {
                        "attributed"
                    } else {
                        "allocated"
                    }),
                ] {
                    current["size_metric"] = mismatched;
                    let encoded = serde_json::to_value(checked_tool_result(
                        operation,
                        Some(metric),
                        current.clone(),
                    ))
                    .unwrap();
                    assert_eq!(encoded["isError"], true);
                    assert_eq!(
                        encoded["structuredContent"]["error"]["code"],
                        "backend_upgrade_required"
                    );
                }
            }
        }
    }

    #[test]
    fn metric_validation_does_not_change_other_tools() {
        let value = json!({"logical_bytes":1_100,"complete":true});
        let encoded = serde_json::to_value(checked_tool_result(
            "get_scan",
            Some("attributed"),
            value.clone(),
        ))
        .unwrap();
        assert_ne!(encoded.get("isError"), Some(&json!(true)));
        assert_eq!(encoded["structuredContent"], value);
    }
}
