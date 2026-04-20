use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DinghyWorkspaceConfig {
    #[serde(default)]
    pub forward_cfgs: Vec<String>,
    #[serde(default)]
    pub test_attribute_aliases: BTreeMap<String, String>,
}

impl DinghyWorkspaceConfig {
    pub fn from_workspace_metadata(
        value: &serde_json::Value,
    ) -> Result<Self> {
        let Some(dinghy) = value.get("dinghy") else {
            return Ok(Self::default());
        };
        serde_json::from_value(dinghy.clone()).context(
            "Failed to parse [workspace.metadata.dinghy]. \
             Known fields: `forward-cfgs`, `test-attribute-aliases`.",
        )
    }
}
