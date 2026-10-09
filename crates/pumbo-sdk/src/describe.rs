//! Machine-readable description of a plugin (plan §6.6.5): config schema,
//! admin actions, metrics and bus topics. The host builds `/pumbo
//! <short-name> <action>` from the actions, validates config files against the
//! schema and exports declared metrics.

use crate::bindings::pumbo::prox::admin::{
    ActionParam, AdminAction, MetricDesc, MetricKind, ParamKind, PluginDescription, TopicDesc,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Description(PluginDescription);

impl Default for Description {
    fn default() -> Self {
        Description::new()
    }
}

impl Description {
    pub fn new() -> Description {
        Description(PluginDescription {
            config_schema: String::new(),
            actions: Vec::new(),
            metrics: Vec::new(),
            topics: Vec::new(),
        })
    }

    /// JSON Schema of the config type (from `schemars`). Mark secret fields
    /// with `#[schemars(extend("x-pumbo-secret" = true))]`.
    pub fn config<T: schemars::JsonSchema>(mut self) -> Description {
        self.0.config_schema = serde_json::to_string(&schemars::schema_for!(T)).unwrap_or_default();
        self
    }

    pub fn action(mut self, a: Action) -> Description {
        self.0.actions.push(a.0);
        self
    }

    fn metric(
        mut self,
        name: &str,
        kind: MetricKind,
        unit: &str,
        labels: &[&str],
        key: &str,
    ) -> Description {
        self.0.metrics.push(MetricDesc {
            name: name.to_string(),
            kind,
            unit: unit.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            description_key: key.to_string(),
        });
        self
    }

    pub fn counter(self, name: &str, unit: &str, labels: &[&str], key: &str) -> Description {
        self.metric(name, MetricKind::Counter, unit, labels, key)
    }

    pub fn gauge(self, name: &str, unit: &str, labels: &[&str], key: &str) -> Description {
        self.metric(name, MetricKind::Gauge, unit, labels, key)
    }

    pub fn histogram(self, name: &str, unit: &str, labels: &[&str], key: &str) -> Description {
        self.metric(name, MetricKind::Histogram, unit, labels, key)
    }

    /// A published topic with the schema of its payload.
    pub fn topic<T: schemars::JsonSchema>(mut self, topic: &str, key: &str) -> Description {
        self.0.topics.push(TopicDesc {
            topic: topic.to_string(),
            payload_schema: serde_json::to_string(&schemars::schema_for!(T)).unwrap_or_default(),
            description_key: key.to_string(),
        });
        self
    }

    pub fn as_wit(&self) -> &PluginDescription {
        &self.0
    }

    pub fn into_wit(self) -> PluginDescription {
        self.0
    }
}

/// An admin action, also a subcommand `/pumbo <short-name> <name>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Action(pub AdminAction);

impl Action {
    /// `permission`: `pumbo.<plugin>.<action>`; the host checks it first.
    pub fn new(name: &str, permission: &str) -> Action {
        Action(AdminAction {
            name: name.to_string(),
            params: Vec::new(),
            permission: permission.to_string(),
            dangerous: false,
            sensitive: false,
            description_key: String::new(),
        })
    }

    pub fn param(mut self, name: &str, kind: ParamKind, required: bool, key: &str) -> Action {
        self.0.params.push(ActionParam {
            name: name.to_string(),
            kind,
            required,
            choices: Vec::new(),
            description_key: key.to_string(),
        });
        self
    }

    pub fn choice(mut self, name: &str, choices: &[&str], required: bool, key: &str) -> Action {
        self.0.params.push(ActionParam {
            name: name.to_string(),
            kind: ParamKind::Choice,
            required,
            choices: choices.iter().map(|c| c.to_string()).collect(),
            description_key: key.to_string(),
        });
        self
    }

    pub fn dangerous(mut self) -> Action {
        self.0.dangerous = true;
        self
    }

    pub fn sensitive(mut self) -> Action {
        self.0.sensitive = true;
        self
    }

    pub fn description(mut self, key: &str) -> Action {
        self.0.description_key = key.to_string();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(schemars::JsonSchema, serde::Deserialize, Default)]
    #[allow(dead_code)]
    struct Cfg {
        greeting: String,
        #[schemars(extend("x-pumbo-secret" = true))]
        api_key: String,
        #[schemars(range(min = 1, max = 10))]
        level: u8,
    }

    #[test]
    fn description_matches_types() {
        let d = Description::new()
            .config::<Cfg>()
            .action(Action::new("reset", "pumbo.example.reset").param(
                "target",
                ParamKind::Player,
                true,
                "a.b",
            ))
            .counter("hellos", "", &["server"], "m.hellos")
            .into_wit();
        let schema: serde_json::Value = serde_json::from_str(&d.config_schema).unwrap();
        let props = &schema["properties"];
        assert_eq!(props["greeting"]["type"], "string");
        assert_eq!(props["api_key"]["x-pumbo-secret"], true);
        assert_eq!(props["level"]["maximum"], 10);
        assert_eq!(d.actions[0].params[0].kind, ParamKind::Player);
        assert_eq!(d.metrics[0].labels, vec!["server".to_string()]);
    }
}
