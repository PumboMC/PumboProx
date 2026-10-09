//! WIT `text` to components, with templates resolved for one recipient.

use pumbo_text::Component;
use pumbo_text::template::{Template, escape_mini};

use crate::HostInner;
use crate::actor::PluginSlot;
use crate::config::Unresolved;
use crate::wit::types::{PlayerId, Text, TextTemplate};

/// Largest text the host encodes (plan §5.8.3).
pub const MAX_TEXT: usize = 32 * 1024;

fn limit(s: &str) -> &str {
    if s.len() <= MAX_TEXT {
        return s;
    }
    let mut end = MAX_TEXT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

pub(crate) fn component(
    host: &HostInner,
    text: &Text,
    player: Option<PlayerId>,
    caller: Option<&PluginSlot>,
) -> Component {
    match text {
        Text::Plain(s) => Component::text(limit(s)),
        Text::Legacy(s) => pumbo_text::parse_legacy(limit(s)),
        Text::Mini(s) => host.message(limit(s)),
        Text::Json(s) => {
            Component::from_json(limit(s)).unwrap_or_else(|_| Component::text("(invalid text)"))
        }
        Text::Template(t) => host.message(limit(&render_cached(host, t, player, caller))),
    }
}

/// Renders a template with push values and cached pull values only (no
/// waiting); used where the host must not wait for a plugin.
pub(crate) fn render_cached(
    host: &HostInner,
    t: &TextTemplate,
    player: Option<PlayerId>,
    caller: Option<&PluginSlot>,
) -> String {
    match Template::parse(&t.mini) {
        Ok(tpl) => {
            let keep = host.cfg.placeholders.unresolved == Unresolved::Keep;
            tpl.render(&t.args, keep, |p| host.placeholder_now(p, player, caller))
        }
        Err(e) => escape_mini(&format!("({e})")),
    }
}
