//! Example PumboProx plugin, a template for plugin authors:
//!
//! - `/hello [name]`: a translated greeting (language of the player) with the
//!   greeting text from the config, which can differ per server or group
//!   (`servers/<server>.yml`, `groups/<group>.yml`); the count comes from
//!   the service `example:counter@1.0`,
//! - the service `example:counter@1.0` (provider) and its consumer (the same
//!   plugin, through the host, as any other plugin would call it),
//! - placeholders: `%example_hellos%` (push) and `%example_greeting%` (pull,
//!   per context), a suggested alias `%hellos%`,
//! - an online counter in the MOTD,
//! - a permission provider with contexts, entries from its config (off in
//!   the manifest; see `pumbo-example.yml` to turn it on),
//! - a description: config schema, the admin action `reset`, the metric
//!   `hellos`,
//! - tests with the fake host of `pumbo_sdk::testing` (`cargo test`).
//!
//! Build: `cargo build -p pumbo-example --target wasm32-wasip2 --profile plugin`,
//! then copy `pumbo_example.wasm` to `plugins/` (under any name): the
//! manifest (`pumbo-example.yml`), the default config (`assets/config.yml`)
//! and the language files (`lang/`) are built into it
//! ([`pumbo_sdk::embed!`]).
//!
//! License: MIT OR Apache-2.0.

use std::cell::Cell;
use std::collections::BTreeMap;

use pumbo_sdk::bindings::pumbo::prox::admin::ParamKind;
use pumbo_sdk::config::{Config, General, Messages};
use pumbo_sdk::contracts::{CheckOffline, CheckOfflineAnswer};
use pumbo_sdk::lang::Lang;
use pumbo_sdk::{
    Action, Actor, CallReject, Command, CommandEvent, Context, Description, PermissionEntry,
    PermissionSet, PlaceholderRequest, PlayerInfo, ServiceCall, StatusEvent, StatusReply, Text,
    Verdict,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExampleConfig {
    pub general: General,
    pub messages: Messages,
    /// Greeting of `/hello`; overlays per server change it.
    pub greeting: String,
    /// Show the online counter in the MOTD.
    pub motd: bool,
    /// Permission provider data: player name → context → nodes (`-node`
    /// denies). Contexts: `global`, `server=<name>`, `group=<name>`.
    pub permissions: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl Default for ExampleConfig {
    fn default() -> Self {
        ExampleConfig {
            general: General::default(),
            messages: Messages {
                prefix: "<muted>[Example] ".into(),
            },
            greeting: "Hello".into(),
            motd: true,
            permissions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Increment {
    pub by: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Get {}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Count {
    pub value: u64,
}

pumbo_sdk::service! {
    /// `example:counter@1.0`: a shared counter.
    pub service Counter("example:counter", 1, 0) provider CounterApi {
        fn increment(Increment) -> Count;
        fn get(Get) -> Count;
    }
}

pub struct Example {
    pub config: Config<ExampleConfig>,
    pub lang: Lang,
    online: Cell<u32>,
    hellos: Cell<u64>,
    local: Cell<u64>,
}

impl Default for Example {
    fn default() -> Self {
        Example {
            config: Config::new(),
            lang: Lang::new(&[
                ("en", include_str!("../lang/en.yml")),
                ("pl", include_str!("../lang/pl.yml")),
            ]),
            online: Cell::new(0),
            hellos: Cell::new(0),
            local: Cell::new(0),
        }
    }
}

fn context_of(s: &str) -> Option<Context> {
    match s {
        "global" => Some(Context::Global),
        _ => s
            .strip_prefix("server=")
            .map(|v| Context::Server(v.into()))
            .or_else(|| s.strip_prefix("group=").map(|g| Context::Group(g.into()))),
    }
}

impl Example {
    fn apply_config(&self) {
        let c = self.config.get();
        self.lang.set_default(&c.general.language);
        self.lang.set_prefix(&c.messages.prefix);
    }

    /// Entries of a player from the config (permission provider).
    fn entries(&self, name: &str) -> Vec<PermissionEntry> {
        let c = self.config.get();
        let Some(by_ctx) = c.permissions.get(&name.to_ascii_lowercase()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (ctx, nodes) in by_ctx {
            let Some(context) = context_of(ctx) else {
                continue;
            };
            for n in nodes {
                let (value, node) = match n.strip_prefix('-') {
                    Some(n) => (false, n),
                    None => (true, n.as_str()),
                };
                out.push(PermissionEntry {
                    node: node.to_string(),
                    value,
                    context: context.clone(),
                });
            }
        }
        out
    }

    fn publish_count(&self) {
        let _ = pumbo_sdk::placeholders::set(
            "hellos",
            None,
            pumbo_sdk::text::plain(self.hellos.get().to_string()),
            &Context::Global,
        );
    }
}

impl CounterApi for Example {
    async fn increment(&self, _: &ServiceCall, req: Increment) -> Result<Count, String> {
        self.hellos.set(self.hellos.get() + req.by);
        self.publish_count();
        Ok(Count {
            value: self.hellos.get(),
        })
    }

    async fn get(&self, _: &ServiceCall, _: Get) -> Result<Count, String> {
        Ok(Count {
            value: self.hellos.get(),
        })
    }
}

impl pumbo_sdk::Plugin for Example {
    async fn init(&self) -> Result<(), String> {
        self.config.reload()?;
        self.lang.reload()?;
        self.apply_config();
        self.publish_count();
        Command::new("hello")
            .permission("pumbo.example.hello")
            .usage("/hello [name]")
            .register()
    }

    fn describe(&self) -> Description {
        Description::new()
            .config::<ExampleConfig>()
            .action(
                Action::new("reset", "pumbo.example.reset")
                    .param("to", ParamKind::Integer, false, "admin.reset.to")
                    .description("admin.reset"),
            )
            .counter("hellos", "", &[], "metrics.hellos")
    }

    async fn on_reload(&self) -> Result<(), String> {
        self.config.reload()?;
        self.lang.reload()?;
        self.apply_config();
        Ok(())
    }

    async fn on_admin_action(
        &self,
        action: String,
        args: Vec<(String, String)>,
        by: Actor,
    ) -> Result<Text, Text> {
        if action != "reset" {
            return Err(pumbo_sdk::text::mini("<err>Unknown action"));
        }
        let to = args
            .iter()
            .find(|(k, _)| k == "to")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        self.hellos.set(to);
        self.publish_count();
        let player = match by {
            Actor::Player(p) => Some(p),
            _ => None,
        };
        Ok(self.lang.text(player, "admin.reset", &[]))
    }

    async fn on_command(&self, e: CommandEvent) {
        if e.name != "hello" {
            return;
        }
        let Some(p) = e.player else {
            pumbo_sdk::log::info(&self.lang.raw("en", "hello.console").unwrap_or_default());
            return;
        };
        let name = match e.args.first() {
            Some(n) => n.clone(),
            None => pumbo_sdk::players::get(p)
                .map(|i| i.profile.name)
                .unwrap_or_default(),
        };
        // Consumer side: any plugin calls the service like this.
        let count = match Counter::client().increment(&Increment { by: 1 }).await {
            Ok(c) => c.value,
            Err(_) => {
                pumbo_sdk::players::send_message(
                    p,
                    self.lang.text(Some(p), "hello.no-service", &[]),
                );
                self.local.set(self.local.get() + 1);
                self.local.get()
            }
        };
        let _ = pumbo_sdk::metrics::counter_add("hellos", 1, &[]);
        let greeting = self.config.for_player(p).greeting.clone();
        let msg = self.lang.text(
            Some(p),
            "hello.greeting",
            &[("greeting", &greeting), ("name", &name), ("count", &count)],
        );
        pumbo_sdk::players::send_message(p, msg);
    }

    async fn on_login(&self, _: PlayerInfo) -> Verdict {
        self.online.set(self.online.get() + 1);
        Verdict::Allow
    }

    async fn on_disconnect(&self, _: u64) {
        self.online.set(self.online.get().saturating_sub(1));
    }

    async fn on_status(&self, mut e: StatusEvent) -> StatusReply {
        if !self.config.get().motd {
            return StatusReply::Keep;
        }
        e.motd = self
            .lang
            .text(None, "motd.line", &[("online", &self.online.get())]);
        StatusReply::Change(e)
    }

    async fn on_service_call(&self, c: ServiceCall) -> Result<Vec<u8>, CallReject> {
        if c.service == pumbo_sdk::contracts::PERMISSIONS.name {
            // check-offline of the permission provider: entries by UUID are
            // not kept in this example, only by name, so it never knows.
            let req: CheckOffline =
                pumbo_sdk::service::from_cbor(&c.payload).map_err(CallReject::Rejected)?;
            let _ = req;
            return pumbo_sdk::service::to_cbor(&CheckOfflineAnswer { value: None })
                .map_err(CallReject::Rejected);
        }
        Counter::dispatch(self, c).await
    }

    async fn on_placeholder(&self, reqs: Vec<PlaceholderRequest>) -> Vec<Option<Text>> {
        reqs.iter()
            .map(|r| match r.key.as_str() {
                "greeting" => Some(pumbo_sdk::text::plain(
                    self.config.at(&r.context).greeting.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    async fn on_permission_load(&self, p: PlayerInfo) -> Result<PermissionSet, String> {
        Ok(PermissionSet {
            entries: self.entries(&p.profile.name),
        })
    }
}

pumbo_sdk::plugin!(Example);
pumbo_sdk::embed!(
    manifest = "pumbo-example.yml",
    config = "assets/config.yml",
    lang = ["lang/en.yml", "lang/pl.yml"],
);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use pumbo_sdk::Plugin as _;
    use pumbo_sdk::testing::{self, block_on, command};

    use super::*;

    #[test]
    fn hello_uses_config_overlay_and_language() {
        testing::reset();
        testing::config_file("config.yml", "greeting: Hi\n");
        testing::config_file("servers/survival.yml", "greeting: Welcome to survival\n");
        let plugin = Example::default();
        block_on(plugin.init()).unwrap();
        assert_eq!(testing::with(|h| h.commands[0].name.clone()), "hello");

        let p = testing::add_player("Steve", Some("lobby"));
        block_on(plugin.on_command(command(p, "hello", &[])));
        block_on(plugin.on_command(command(p, "hello", &["<b>Alex"])));
        let m = testing::messages(p);
        // Without a provider in the fake host the plugin counts locally.
        assert!(m[0].contains("counter is unavailable"), "{m:?}");
        assert_eq!(
            m[1],
            "<muted>[Example] <p>Hi, <s>Steve</s>! <muted>(hello #1)"
        );
        // Player text goes in as an argument, never as MiniMessage.
        let sent = testing::sent_to(p);
        let pumbo_sdk::testing::Sent::Message(pumbo_sdk::Text::Template(t)) = &sent[3] else {
            panic!("{sent:?}")
        };
        assert!(t.mini.contains("{name}"));
        assert!(t.args.contains(&("name".into(), "<b>Alex".into())));

        // Same plugin, the player moved to survival: the overlay applies.
        testing::with(|h| {
            let pl = h.players.get_mut(&p).unwrap();
            pl.context.server = Some("survival".into());
        });
        block_on(plugin.on_command(command(p, "hello", &[])));
        assert!(
            testing::messages(p)
                .last()
                .unwrap()
                .contains("Welcome to survival")
        );
    }

    #[test]
    fn counter_service_dispatch_and_placeholders() {
        testing::reset();
        let plugin = Example::default();
        block_on(plugin.init()).unwrap();
        // Provider side, as the host would call it.
        let payload = pumbo_sdk::service::to_cbor(&Increment { by: 2 }).unwrap();
        let out = block_on(plugin.on_service_call(testing::service_call(
            "example:counter",
            1,
            "increment",
            payload,
        )))
        .unwrap();
        let c: Count = pumbo_sdk::service::from_cbor(&out).unwrap();
        assert_eq!(c.value, 2);
        let unknown = block_on(plugin.on_service_call(testing::service_call(
            "example:counter",
            1,
            "nope",
            Vec::new(),
        )));
        assert_eq!(unknown, Err(CallReject::UnknownMethod));
        let pushed = testing::with(|h| {
            h.placeholders
                .get(&("hellos".to_string(), None, "global".to_string()))
                .cloned()
        });
        assert_eq!(pushed, Some(pumbo_sdk::text::plain("2")));
    }

    #[test]
    fn counter_in_motd_and_languages_match() {
        testing::reset();
        let plugin = Example::default();
        block_on(plugin.init()).unwrap();
        plugin.lang.check().unwrap();
        let p = testing::add_player("Ania", None);
        block_on(plugin.on_login(pumbo_sdk::players::get(p).unwrap()));
        let e = StatusEvent {
            connection: pumbo_sdk::players::get(p).unwrap().connection,
            motd: pumbo_sdk::text::plain("x"),
            online: 0,
            max: 0,
            favicon: false,
        };
        let StatusReply::Change(e) = block_on(plugin.on_status(e)) else {
            panic!("no change")
        };
        assert!(testing::render(&e.motd).contains("1</s> online"));
        let schema = plugin.describe().into_wit().config_schema;
        assert!(schema.contains("greeting"));
    }

    #[test]
    fn permission_provider_with_contexts() {
        testing::reset();
        testing::config_file(
            "config.yml",
            "permissions:\n  steve:\n    global: [pumbo.example.hello]\n    server=survival: [\"-pumbo.example.hello\", pumbo.example.reset]\n",
        );
        let plugin = Example::default();
        block_on(plugin.init()).unwrap();
        let p = testing::add_player("Steve", None);
        let set = block_on(plugin.on_permission_load(pumbo_sdk::players::get(p).unwrap())).unwrap();
        assert_eq!(set.entries.len(), 3);
        assert!(set.entries.contains(&PermissionEntry {
            node: "pumbo.example.hello".into(),
            value: false,
            context: Context::Server("survival".into()),
        }));
    }
}
