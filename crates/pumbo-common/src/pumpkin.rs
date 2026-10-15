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

/// Exact nodes of the Pumbo plugins in Pumpkin form, without the ones a config
/// adds (`pumbobans:limit.<group>`). PumboBridge sends them in its catalog and
/// PumboPerms on Pumpkin decides them, so a node under a wildcard Pumpkin
/// cannot read still gets its own value. Each plugin core tests its part.
pub const PUMBO_NODES: &[&str] = &[
    "pumboauth:command",
    "pumboauth:forcechangepassword",
    "pumboauth:forcelogin",
    "pumboauth:forceoffline",
    "pumboauth:forcepremium",
    "pumboauth:forceregister",
    "pumboauth:import",
    "pumboauth:logout",
    "pumboauth:player",
    "pumboauth:reload",
    "pumboauth:reset2fa",
    "pumboauth:stats",
    "pumboauth:unblock",
    "pumboauth:unregister",
    "pumboauth:version",
    "pumbobans:admin.import",
    "pumbobans:admin.purge",
    "pumbobans:admin.reload",
    "pumbobans:admin.version",
    "pumbobans:alts",
    "pumbobans:ban",
    "pumbobans:banlist",
    "pumbobans:check",
    "pumbobans:command",
    "pumbobans:exempt.ban",
    "pumbobans:exempt.bypass",
    "pumbobans:exempt.kick",
    "pumbobans:exempt.mute",
    "pumbobans:exempt.warn",
    "pumbobans:history",
    "pumbobans:ipban",
    "pumbobans:ipmute",
    "pumbobans:kick",
    "pumbobans:mute",
    "pumbobans:mutelist",
    "pumbobans:notify",
    "pumbobans:notify.alts",
    "pumbobans:notify.silent",
    "pumbobans:staffhistory",
    "pumbobans:tempban",
    "pumbobans:tempmute",
    "pumbobans:unban",
    "pumbobans:unbanip",
    "pumbobans:unmute",
    "pumbobans:unwarn",
    "pumbobans:viewips",
    "pumbobans:warn",
    "pumbobans:warns",
    "pumbofilter:attack",
    "pumbofilter:bypass",
    "pumbofilter:captcha",
    "pumbofilter:command",
    "pumbofilter:reload",
    "pumbofilter:stats",
    "pumbofilter:unverify",
    "pumbofilter:verify",
    "pumbofilter:version",
    "pumboperms:command",
    "pumboperms:editor",
    "pumboperms:export",
    "pumboperms:group.create",
    "pumboperms:group.delete",
    "pumboperms:group.info",
    "pumboperms:group.list",
    "pumboperms:group.listmembers",
    "pumboperms:group.meta.info",
    "pumboperms:group.meta.set",
    "pumboperms:group.meta.unset",
    "pumboperms:group.parent.add",
    "pumboperms:group.parent.info",
    "pumboperms:group.parent.remove",
    "pumboperms:group.parent.set",
    "pumboperms:group.permission.check",
    "pumboperms:group.permission.clear",
    "pumboperms:group.permission.info",
    "pumboperms:group.permission.set",
    "pumboperms:group.permission.unset",
    "pumboperms:group.rename",
    "pumboperms:group.setdisplayname",
    "pumboperms:group.setweight",
    "pumboperms:import",
    "pumboperms:log",
    "pumboperms:log.notify",
    "pumboperms:reload",
    "pumboperms:track.create",
    "pumboperms:track.delete",
    "pumboperms:track.edit",
    "pumboperms:track.info",
    "pumboperms:track.list",
    "pumboperms:user.clear",
    "pumboperms:user.demote",
    "pumboperms:user.info",
    "pumboperms:user.meta.info",
    "pumboperms:user.meta.set",
    "pumboperms:user.meta.unset",
    "pumboperms:user.parent.add",
    "pumboperms:user.parent.info",
    "pumboperms:user.parent.remove",
    "pumboperms:user.parent.set",
    "pumboperms:user.permission.check",
    "pumboperms:user.permission.clear",
    "pumboperms:user.permission.info",
    "pumboperms:user.permission.set",
    "pumboperms:user.permission.unset",
    "pumboperms:user.promote",
    "pumboperms:version",
];

/// Aliases Pumpkin 0.1.0-dev and 0.2.0 run without any permission check, with
/// the node of their command and the operator level Pumpkin gives that node by
/// default: an alias of a command whose root has no executor of its own gets no
/// requirement (fixed upstream after 0.2.0, Pumpkin #3801). Without a guard
/// every player may `/tp`, `/xp`, `/banip` and `/pardonip`.
pub const UNGUARDED_ALIASES: &[(&str, &str, u8)] = &[
    ("tp", "minecraft:command.teleport", 2),
    ("xp", "minecraft:command.experience", 2),
    ("banip", "minecraft:command.banip", 3),
    ("pardonip", "minecraft:command.pardonip", 3),
];

/// The node a player needs for this command line (without `/`) when Pumpkin
/// itself does not check it ([`UNGUARDED_ALIASES`]); `None` for anything else.
/// Looks at the first word and, in `/execute`, at the word after every `run`,
/// because Pumpkin does not check the target of `execute ... run` either.
pub fn unguarded_alias(command: &str) -> Option<&'static str> {
    let mut words = command.trim_start().trim_start_matches('/').split(' ').filter(|w| !w.is_empty());
    let first = words.next()?;
    let mut candidates = vec![first];
    if first.eq_ignore_ascii_case("execute") {
        let rest: Vec<&str> = words.collect();
        candidates.extend(rest.windows(2).filter_map(|w| match w {
            [run, next] if *run == "run" => Some(*next),
            _ => None,
        }));
    }
    candidates
        .into_iter()
        .find_map(|word| UNGUARDED_ALIASES.iter().find(|(a, _, _)| word.eq_ignore_ascii_case(a)).map(|(_, n, _)| *n))
}

/// The operator level Pumpkin gives an [`UNGUARDED_ALIASES`] node by default
/// (4 for any other node).
pub fn alias_op_level(node: &str) -> u8 {
    UNGUARDED_ALIASES.iter().find(|(_, n, _)| *n == node).map_or(4, |(_, _, l)| *l)
}

/// Commands the other Pumbo plugins register on Pumpkin by default, with the
/// plugin. Pumpkin registers a second command of the same name over the first
/// without a word, so PumboPerms does not take these as short commands. Each
/// plugin core tests its part.
pub const PUMBO_COMMANDS: &[(&str, &[&str])] = &[
    (
        "PumboAuth",
        &[
            "pumboauth",
            "pa",
            "auth",
            "login",
            "l",
            "log",
            "register",
            "reg",
            "changepassword",
            "changepass",
            "cp",
            "unregister",
            "unreg",
            "logout",
            "destroysession",
            "2fa",
            "totp",
            "premium",
            "license",
        ],
    ),
    (
        "PumboBans",
        &[
            "pumbobans",
            "pb",
            "ban",
            "tempban",
            "ipban",
            "banip",
            "mute",
            "tempmute",
            "ipmute",
            "warn",
            "kick",
            "unban",
            "unbanip",
            "unmute",
            "unwarn",
            "history",
            "warns",
            "banlist",
            "mutelist",
            "checkban",
            "alts",
            "staffhistory",
        ],
    ),
    ("PumboFilter", &["pumbofilter", "pf", "captcha"]),
];

/// The Pumbo plugin that registers `command` on Pumpkin ([`PUMBO_COMMANDS`]).
pub fn pumbo_command_owner(command: &str) -> Option<&'static str> {
    PUMBO_COMMANDS.iter().find(|(_, names)| names.contains(&command)).map(|(plugin, _)| *plugin)
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
        for (_, n, _) in super::UNGUARDED_ALIASES {
            assert!(super::COMMAND_NODES.contains(n), "{n}");
        }
    }

    #[test]
    fn alias_after_execute_run_is_guarded() {
        use super::unguarded_alias as u;
        assert_eq!(u("execute run tp Steve"), Some("minecraft:command.teleport"));
        assert_eq!(u("execute as @s at @s run banip 1.2.3.4"), Some("minecraft:command.banip"));
        assert_eq!(u("/execute  run execute run xp add Steve 5"), Some("minecraft:command.experience"));
        // `run` as an argument of another command is just a word.
        assert_eq!(u("msg Steve run tp"), None);
        assert_eq!(u("execute run say tp"), None);
        assert_eq!(super::alias_op_level("minecraft:command.banip"), 3);
        assert_eq!(super::alias_op_level("minecraft:command.teleport"), 2);
    }

    #[test]
    fn nodes_are_unique_and_namespaced() {
        let mut seen = std::collections::HashSet::new();
        for n in super::COMMAND_NODES.iter().chain(super::PUMBO_NODES) {
            assert!(n.contains(':') && !n.contains(' ') && *n == n.to_lowercase(), "{n}");
            assert!(seen.insert(n), "{n} twice");
        }
    }
}
