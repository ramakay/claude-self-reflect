use super::*;

#[tokio::test]
async fn blank_search_inputs_are_invalid_params_before_any_search_work() {
    for proxied in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let search = if proxied {
            let client = crate::search::vector_ipc::VectorClient::for_database(
                &dir.path().join("csr-engine.db"),
            )
            .unwrap();
            SearchEngine::proxied(Arc::new(crate::search::vector_ipc::VectorProxy::new(
                client,
                storage.clone(),
            )))
        } else {
            SearchEngine::new(8)
        };
        let search = Arc::new(RwLock::new(search));
        let server = CsrServer::new(
            storage,
            Arc::new(EmbeddingEngine::disabled()),
            search.clone(),
            dir.path().to_path_buf(),
            dir.path().join("index-v3"),
            None,
            None,
        );
        for blank in ["", " ", "\t\n", "\u{2003}"] {
            macro_rules! rejected {
                ($method:ident, $params:ty, $args:expr) => {{
                    let params: $params = serde_json::from_value($args).unwrap();
                    let error = server.$method(Parameters(params)).await.unwrap_err();
                    assert_eq!(
                        error.code,
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        "{} accepted blank input or used the wrong error: {error:?}",
                        stringify!($method)
                    );
                }};
            }
            rejected!(
                reflect_on_past,
                ReflectOnPastParams,
                serde_json::json!({"query": blank})
            );
            rejected!(
                quick_check,
                QuickCheckParams,
                serde_json::json!({"query": blank})
            );
            rejected!(
                search_insights,
                SearchInsightsParams,
                serde_json::json!({"query": blank})
            );
            rejected!(
                search_by_recency,
                SearchByRecencyParams,
                serde_json::json!({"query": blank})
            );
            rejected!(
                search_by_file,
                SearchByFileParams,
                serde_json::json!({"file_path": blank})
            );
            rejected!(
                search_by_concept,
                SearchByConceptParams,
                serde_json::json!({"concept": blank})
            );
            rejected!(
                get_more_results,
                GetMoreResultsParams,
                serde_json::json!({"query": blank})
            );
            rejected!(why, WhyParams, serde_json::json!({"query": blank}));
            rejected!(
                code_graph,
                CodeGraphParams,
                serde_json::json!({"symbol": blank})
            );
            rejected!(
                code_graph,
                CodeGraphParams,
                serde_json::json!({"file": blank})
            );
            rejected!(
                code_graph,
                CodeGraphParams,
                serde_json::json!({"symbol": "valid_symbol", "file": blank})
            );
        }
        if let Some(proxy) = search.read().await.proxy() {
            assert_eq!(proxy.failures(), 0, "validation must precede proxy access");
        };
    }
    for query in ["needle", "  a real query\n", "日本語"] {
        assert!(query_validation::validate(query, "query").is_ok());
    }
}
