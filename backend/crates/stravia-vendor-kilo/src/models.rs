use super::*;

pub(super) fn parse(mut value: Value) -> Result<DiscoverResponse, PluginError> {
    let data = value.get_mut("data").map(Value::take).ok_or_else(|| {
        fail(
            ErrorKind::Invalid,
            "Kilo models response missing data array",
        )
    })?;
    let Value::Array(data) = data else {
        return Err(fail(
            ErrorKind::Invalid,
            "Kilo models response data must be an array",
        ));
    };
    let mut models = Vec::with_capacity(data.len());
    for model in data {
        let id = model
            .get("id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo model ID missing"))?
            .to_owned();
        let has = |field: &str, value: &str| {
            model
                .pointer(field)
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(value)))
        };
        if has("/architecture/output_modalities", "image") {
            continue;
        }
        let mut capabilities = Vec::new();
        if has("/architecture/input_modalities", "image") {
            capabilities.push("image_input".into());
        }
        if has("/supported_parameters", "reasoning") {
            capabilities.push("reasoning".into());
        }
        if has("/supported_parameters", "tools") {
            capabilities.push("tool_call".into());
        }
        let display_name = model
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_owned();
        let family = model
            .pointer("/opencode/family")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Value::Object(fields) = model else {
            return Err(fail(ErrorKind::Invalid, "Invalid Kilo model"));
        };
        let mut metadata: BTreeMap<_, _> = fields.into_iter().collect();
        if metadata
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| {
                description.len() > 4096 || description.chars().any(char::is_control)
            })
        {
            // 平台规格说明只接受有界单行文本；原始目录 Markdown 保留在来源空间，
            // 不让合法多行说明阻断整份清单，也不截断或伪造原文。
            let description = metadata.remove("description").expect("checked description");
            metadata.insert(
                "kilo".into(),
                serde_json::json!({"description":description}),
            );
        }
        models.push(DiscoveredModel {
            id: id.clone(),
            display_name,
            family,
            selector: Some(id),
            capabilities,
            metadata,
        });
    }
    Ok(DiscoverResponse {
        models,
        next_cursor: None,
    })
}
pub(super) fn discover(
    host: &GuestHost,
    provider: &ProviderSnapshot,
) -> Result<DiscoverResponse, PluginError> {
    let path = organization(provider).map_or_else(
        || "/api/gateway/models".into(),
        |org| format!("/api/organizations/{}/models", escape(org)),
    );
    token(provider)?;
    parse(http_json(host, provider, "GET", &path)?)
}
