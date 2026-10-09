//! Permission nodes Pumpkin checks for its own commands (Pumpkin 0.2.0; the
//! same list serves 0.1.0-dev, where a node it does not know is harmless).
//!
//! One list for every Pumbo plugin that writes Pumpkin permissions: PumboPerms
//! sets them from its groups, PumboBridge from the proxy's table (it sends
//! them as its catalog). Nodes of later Pumpkin versions still work through
//! wildcards (`minecraft:command.*`) or when they appear in the data.

/// Built-in command nodes.
pub const COMMAND_NODES: &[&str] = &[
    "minecraft:command.advancement",
    "minecraft:command.attribute",
    "minecraft:command.ban",
    "minecraft:command.banip",
    "minecraft:command.banlist",
    "minecraft:command.bossbar",
    "minecraft:command.clear",
    "minecraft:command.clone",
    "minecraft:command.compute",
    "minecraft:command.damage",
    "minecraft:command.data",
    "minecraft:command.datapack",
    "minecraft:command.debug",
    "minecraft:command.defaultgamemode",
    "minecraft:command.deop",
    "minecraft:command.dialog",
    "minecraft:command.difficulty",
    "minecraft:command.effect",
    "minecraft:command.enchant",
    "minecraft:command.execute",
    "minecraft:command.experience",
    "minecraft:command.fetchprofile",
    "minecraft:command.fill",
    "minecraft:command.fillbiome",
    "minecraft:command.forceload",
    "minecraft:command.function",
    "minecraft:command.gamemode",
    "minecraft:command.gamerule",
    "minecraft:command.give",
    "minecraft:command.help",
    "minecraft:command.item",
    "minecraft:command.kick",
    "minecraft:command.kill",
    "minecraft:command.list",
    "minecraft:command.locate",
    "minecraft:command.loot",
    "minecraft:command.me",
    "minecraft:command.msg",
    "minecraft:command.op",
    "minecraft:command.pardon",
    "minecraft:command.pardonip",
    "minecraft:command.particle",
    "minecraft:command.place",
    "minecraft:command.playsound",
    "minecraft:command.posteffect",
    "minecraft:command.raid",
    "minecraft:command.random",
    "minecraft:command.random.reset",
    "minecraft:command.recipe",
    "minecraft:command.reload",
    "minecraft:command.return",
    "minecraft:command.ride",
    "minecraft:command.rotate",
    "minecraft:command.say",
    "minecraft:command.schedule",
    "minecraft:command.scoreboard",
    "minecraft:command.seed",
    "minecraft:command.selector",
    "minecraft:command.setblock",
    "minecraft:command.setidletimeout",
    "minecraft:command.setworldspawn",
    "minecraft:command.spawnpoint",
    "minecraft:command.spectate",
    "minecraft:command.spreadplayers",
    "minecraft:command.stop",
    "minecraft:command.stopsound",
    "minecraft:command.stopwatch",
    "minecraft:command.summon",
    "minecraft:command.swing",
    "minecraft:command.tag",
    "minecraft:command.team",
    "minecraft:command.teammsg",
    "minecraft:command.teleport",
    "minecraft:command.tellraw",
    "minecraft:command.test",
    "minecraft:command.tick",
    "minecraft:command.time",
    "minecraft:command.title",
    "minecraft:command.transfer",
    "minecraft:command.trigger",
    "minecraft:command.waypoint",
    "minecraft:command.weather",
    "minecraft:command.whitelist",
    "minecraft:command.worldborder",
    "pumpkin:command.plugin",
    "pumpkin:command.plugins",
    "pumpkin:command.pumpkin",
    "pumpkin:command.tps",
];

/// Aliases Pumpkin 0.1.0-dev and 0.2.0 run without any permission check, with
/// the node of their command: an alias of a command whose root has no executor
/// of its own gets no requirement (fixed upstream after 0.2.0, Pumpkin #3801).
/// Without a guard every player may `/tp`, `/xp`, `/banip` and `/pardonip`.
pub const UNGUARDED_ALIASES: &[(&str, &str)] = &[
    ("tp", "minecraft:command.teleport"),
    ("xp", "minecraft:command.experience"),
    ("banip", "minecraft:command.banip"),
    ("pardonip", "minecraft:command.pardonip"),
];

/// The node a player needs for this command line (without `/`) when Pumpkin
/// itself does not check it ([`UNGUARDED_ALIASES`]); `None` for anything else.
pub fn unguarded_alias(command: &str) -> Option<&'static str> {
    let word = command.trim_start().trim_start_matches('/').split(' ').next()?;
    UNGUARDED_ALIASES.iter().find(|(a, _)| word.eq_ignore_ascii_case(a)).map(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn aliases_pumpkin_leaves_open_need_their_command_node() {
        use super::unguarded_alias as u;
        assert_eq!(u("tp Steve Alex"), Some("minecraft:command.teleport"));
        assert_eq!(u("/TP 1 2 3"), Some("minecraft:command.teleport"));
        assert_eq!(u("xp add Steve 5 levels"), Some("minecraft:command.experience"));
        assert_eq!(u("banip 1.2.3.4"), Some("minecraft:command.banip"));
        assert_eq!(u("pardonip 1.2.3.4"), Some("minecraft:command.pardonip"));
        // Checked by Pumpkin itself.
        for c in ["teleport Steve", "tpa Steve", "help", "h", "?", "msg Steve tp", "", "experience add"] {
            assert_eq!(u(c), None, "{c}");
        }
        for (_, n) in super::UNGUARDED_ALIASES {
            assert!(super::COMMAND_NODES.contains(n), "{n}");
        }
    }

    #[test]
    fn nodes_are_unique_and_namespaced() {
        let mut seen = std::collections::HashSet::new();
        for n in super::COMMAND_NODES {
            assert!(n.contains(':') && !n.contains(' ') && *n == n.to_lowercase(), "{n}");
            assert!(seen.insert(n), "{n} twice");
        }
    }
}
