//! JSON and NBT layouts before and from 1.21.5 (protocol 770), round trips,
//! limits and robustness.

use pumbo_nbt::{Compound, List, Tag};
use pumbo_text::{
    Argument, ClickEvent, Color, Component, Content, HoverEvent, NamedColor, NbtContent, Style,
    TextError, TextFormat, parse_legacy, parse_mini,
};
use serde_json::{Value, json};

fn rich() -> Component {
    let hover = Component::text("tip").color(Color::Named(NamedColor::Gold));
    Component {
        content: Content::Text("Hello ".into()),
        style: Style {
            color: Some(Color::Rgb(0x12AB34)),
            shadow_color: Some(-16_777_216),
            bold: Some(true),
            italic: Some(false),
            click_event: Some(ClickEvent::RunCommand("/server lobby".into())),
            hover_event: Some(HoverEvent::ShowText(Box::new(hover))),
            insertion: Some("ins".into()),
            font: Some("minecraft:uniform".into()),
            ..Style::default()
        },
        extra: vec![
            Component::text("plain"),
            Component::translatable(
                "chat.type.text",
                vec![
                    Component::text("Steve"),
                    Component::text("hi").color(Color::Named(NamedColor::Red)),
                ],
            ),
            Component {
                content: Content::Keybind("key.inventory".into()),
                ..Component::default()
            },
            Component {
                content: Content::Score {
                    name: "*".into(),
                    objective: "kills".into(),
                },
                ..Component::default()
            },
            Component {
                content: Content::Selector {
                    pattern: "@p".into(),
                    separator: Some(Box::new(Component::text(", "))),
                },
                ..Component::default()
            },
            Component {
                content: Content::Nbt(NbtContent {
                    path: "Pos".into(),
                    interpret: Some(false),
                    entity: Some("@s".into()),
                    ..NbtContent::default()
                }),
                ..Component::default()
            },
            Component::text("page").with_style(Style {
                click_event: Some(ClickEvent::ChangePage(3)),
                hover_event: Some(HoverEvent::ShowEntity {
                    entity_type: "minecraft:pig".into(),
                    uuid: [1, 2, 3, 4],
                    name: Some(Box::new(Component::text("Pig"))),
                }),
                ..Style::default()
            }),
            Component::text("item").with_style(Style {
                click_event: Some(ClickEvent::OpenUrl("https://example.com".into())),
                hover_event: Some(HoverEvent::ShowItem {
                    id: "minecraft:stone".into(),
                    count: Some(2),
                    components: None,
                }),
                ..Style::default()
            }),
        ],
    }
}

#[test]
fn round_trips_in_every_format() {
    let c = rich();
    for f in [TextFormat::V769, TextFormat::V770] {
        assert_eq!(Component::from_json(&c.to_json(f)).unwrap(), c, "{f:?}");
        let nbt = c.to_nbt(f);
        assert_eq!(Component::from_nbt(&nbt).unwrap(), c, "{f:?}");
        // Through the wire bytes as well.
        let mut bytes = Vec::new();
        pumbo_nbt::write_network(&mut bytes, Some(&nbt)).unwrap();
        let back = pumbo_nbt::read_network(&mut bytes.as_slice(), pumbo_nbt::Limits::BACKEND)
            .unwrap()
            .unwrap();
        assert_eq!(Component::from_nbt(&back).unwrap(), c);
    }
    // 767 has no shadow color: the field is left out.
    let without = Component::from_json(&c.to_json(TextFormat::V767)).unwrap();
    assert_eq!(without.style.shadow_color, None);
    let mut expected = c.clone();
    expected.style.shadow_color = None;
    assert_eq!(without, expected);
}

#[test]
fn layout_before_770() {
    let v = rich().to_json_value(TextFormat::V769);
    assert_eq!(
        v["clickEvent"],
        json!({"action": "run_command", "value": "/server lobby"})
    );
    assert_eq!(v["hoverEvent"]["action"], "show_text");
    assert_eq!(
        v["hoverEvent"]["contents"],
        json!({"text": "tip", "color": "gold"})
    );
    assert_eq!(v["color"], "#12AB34");
    assert_eq!(v["bold"], true);
    assert_eq!(v["shadow_color"], -16_777_216);
    assert!(v.get("click_event").is_none());
    let page = &v["extra"][6];
    assert_eq!(
        page["clickEvent"],
        json!({"action": "change_page", "value": "3"})
    );
    assert_eq!(
        page["hoverEvent"],
        json!({"action": "show_entity", "contents": {"name": "Pig", "type": "minecraft:pig", "id": [1, 2, 3, 4]}})
    );
    let item = &v["extra"][7];
    assert_eq!(
        item["hoverEvent"],
        json!({"action": "show_item", "contents": {"id": "minecraft:stone", "count": 2}})
    );
    assert_eq!(item["clickEvent"]["value"], "https://example.com");
    assert_eq!(v["extra"][0], "plain", "plain text is a bare string");
}

#[test]
fn layout_from_770() {
    let v = rich().to_json_value(TextFormat::V770);
    assert_eq!(
        v["click_event"],
        json!({"action": "run_command", "command": "/server lobby"})
    );
    assert_eq!(
        v["hover_event"],
        json!({"action": "show_text", "value": {"text": "tip", "color": "gold"}})
    );
    assert!(v.get("clickEvent").is_none());
    let page = &v["extra"][6];
    assert_eq!(
        page["click_event"],
        json!({"action": "change_page", "page": 3})
    );
    assert_eq!(
        page["hover_event"],
        json!({"action": "show_entity", "name": "Pig", "id": "minecraft:pig", "uuid": [1, 2, 3, 4]})
    );
    let item = &v["extra"][7];
    assert_eq!(
        item["hover_event"],
        json!({"action": "show_item", "id": "minecraft:stone", "count": 2})
    );
    assert_eq!(item["click_event"]["url"], "https://example.com");
}

#[test]
fn nbt_types_and_mixed_lists() {
    let c = rich();
    let Tag::Compound(root) = c.to_nbt(TextFormat::V770) else {
        panic!("not a compound");
    };
    assert_eq!(root.get("bold"), Some(&Tag::Byte(1)));
    assert_eq!(root.get("shadow_color"), Some(&Tag::Int(-16_777_216)));
    // `extra` mixes a bare string with compounds: every element becomes a
    // compound, the string wrapped as {"": "plain"}.
    let Some(Tag::List(extra)) = root.get("extra") else {
        panic!("no extra");
    };
    assert_eq!(extra.element, pumbo_nbt::id::COMPOUND);
    assert_eq!(
        extra.items.first(),
        Some(&Tag::Compound(Compound(vec![(
            String::new(),
            Tag::String("plain".into())
        )])))
    );
    let Some(Tag::Compound(page)) = extra.items.get(6) else {
        panic!("no page");
    };
    let Some(Tag::Compound(hover)) = page.get("hover_event") else {
        panic!("no hover");
    };
    assert_eq!(hover.get("uuid"), Some(&Tag::IntArray(vec![1, 2, 3, 4])));
    // Plain text is a bare string tag.
    assert_eq!(
        Component::text("x").to_nbt(TextFormat::V770),
        Tag::String("x".into())
    );
    // A list of strings only stays a list of strings.
    let two = Component::text("a").append(Component::text("b"));
    let Tag::Compound(two) = two.to_nbt(TextFormat::V770) else {
        panic!();
    };
    assert_eq!(
        two.get("extra"),
        Some(&Tag::List(List::new(
            pumbo_nbt::id::STRING,
            vec![Tag::String("b".into())]
        )))
    );
}

#[test]
fn reads_vanilla_shapes() {
    // List form: the rest are children of the first.
    let c = Component::from_json(r#"["A", {"text": "B", "color": "red"}, "C"]"#).unwrap();
    assert_eq!(c.plain_text(), "ABC");
    assert_eq!(c.extra.len(), 2);
    // Translation arguments may be numbers and booleans.
    let c = Component::from_json(r#"{"translate": "x", "with": ["a", 3, 2.5, true]}"#).unwrap();
    let Content::Translatable { with, .. } = &c.content else {
        panic!();
    };
    let args: Vec<String> = with.iter().map(Argument::plain_text).collect();
    assert_eq!(args, ["a", "3", "2.5", "true"]);
    // Explicit type and fuzzy detection.
    let c = Component::from_json(r#"{"type": "keybind", "keybind": "key.jump"}"#).unwrap();
    assert_eq!(c.content, Content::Keybind("key.jump".into()));
    let c = Component::from_json(r#"{"type": "text", "translate": "k"}"#).unwrap();
    assert!(matches!(c.content, Content::Translatable { .. }));
    // shadow_color as four floats (red, green, blue, alpha).
    let c = Component::from_json(r#"{"text": "", "shadow_color": [1.0, 0.0, 0.0, 1.0]}"#).unwrap();
    assert_eq!(c.style.shadow_color, Some(0xFFFF0000_u32 as i32));
    // Old show_item with only an ID, old deprecated `value` for show_text,
    // UUID as a string, namespaced action names.
    let c = Component::from_json(
        r#"{"text": "x", "hoverEvent": {"action": "show_item", "contents": "minecraft:dirt"},
            "clickEvent": {"action": "minecraft:open_url", "value": "https://a.b"}}"#,
    )
    .unwrap();
    assert_eq!(
        c.style.hover_event,
        Some(HoverEvent::ShowItem {
            id: "minecraft:dirt".into(),
            count: None,
            components: None
        })
    );
    assert_eq!(
        c.style.click_event,
        Some(ClickEvent::OpenUrl("https://a.b".into()))
    );
    let c = Component::from_json(
        r#"{"text": "x", "hoverEvent": {"action": "show_entity",
            "contents": {"type": "minecraft:cow", "id": "00000000-0000-0001-0000-000000000002"}}}"#,
    )
    .unwrap();
    assert!(matches!(
        c.style.hover_event,
        Some(HoverEvent::ShowEntity {
            uuid: [0, 1, 0, 2],
            ..
        })
    ));
    // Unknown actions and 1.21.6 actions are kept.
    let c = Component::from_json(
        r#"{"text": "x", "click_event": {"action": "custom", "id": "a:b", "payload": {"k": 1}},
            "hover_event": {"action": "show_future", "data": 5}}"#,
    )
    .unwrap();
    let back = Component::from_json(&c.to_json(TextFormat::V770)).unwrap();
    assert_eq!(back, c);
    // 1.21.9 object content is kept as raw fields.
    let c = Component::from_json(r#"{"type": "object", "sprite": "block/stone", "color": "red"}"#)
        .unwrap();
    assert!(matches!(&c.content, Content::Object(f) if f.len() == 1));
    let v = c.to_json_value(TextFormat::V770);
    assert_eq!(v, json!({"sprite": "block/stone", "color": "red"}));
}

#[test]
fn old_layout_converts_to_new() {
    let old = r#"{"text": "x", "clickEvent": {"action": "change_page", "value": "7"},
        "hoverEvent": {"action": "show_text", "contents": "tip"}}"#;
    let v = Component::from_json(old)
        .unwrap()
        .to_json_value(TextFormat::V770);
    assert_eq!(
        v["click_event"],
        json!({"action": "change_page", "page": 7})
    );
    assert_eq!(
        v["hover_event"],
        json!({"action": "show_text", "value": "tip"})
    );
}

#[test]
fn rejects_invalid_input() {
    for bad in [
        "",
        "12",
        "true",
        "[]",
        "{}",
        r#"{"text": 5}"#,
        r#"{"text": "a", "color": "pink"}"#,
        r#"{"text": "a", "extra": []}"#,
        r#"{"text": "a", "clickEvent": {"value": "x"}}"#,
        r#"{"text": "a", "clickEvent": {"action": "change_page", "value": "x"}}"#,
        r#"{"text": "a", "hoverEvent": {"action": "show_entity", "contents": {"type": "a", "id": [1, 2]}}}"#,
    ] {
        assert!(Component::from_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn deep_nesting_is_bounded() {
    let mut tag = Tag::String("x".into());
    for _ in 0..400 {
        tag = Tag::Compound(Compound(vec![
            ("text".into(), Tag::String(String::new())),
            ("extra".into(), Tag::List(List::of(vec![tag]))),
        ]));
    }
    assert!(matches!(
        Component::from_nbt(&tag),
        Err(TextError::TooDeep(_))
    ));
    let mut json = String::from("\"x\"");
    for _ in 0..1000 {
        json = format!("[{json}]");
    }
    assert!(Component::from_json(&json).is_err());
}

#[test]
fn legacy_and_mini_meet_the_model() {
    let a = parse_legacy("&cHi &lthere");
    let b = parse_mini("<red>Hi <bold>there");
    assert_eq!(a.plain_text(), b.plain_text());
    assert_eq!(
        Component::from_json(&b.to_json(TextFormat::V770)).unwrap(),
        b
    );
    let v = b.to_json_value(TextFormat::V770);
    assert_eq!(
        v["extra"][1],
        json!({"text": "there", "color": "red", "bold": true})
    );
    assert_eq!(Value::from(""), v["text"]);
}

mod props {
    use super::*;
    use proptest::prelude::*;

    fn color() -> impl Strategy<Value = Color> {
        prop_oneof![
            proptest::sample::select(NamedColor::ALL.to_vec()).prop_map(Color::Named),
            (0u32..=0xFF_FFFF).prop_map(Color::Rgb),
        ]
    }

    fn style() -> impl Strategy<Value = Style> {
        (
            proptest::option::of(color()),
            proptest::option::of(any::<i32>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(any::<bool>()),
            proptest::option::of(prop_oneof![
                ".{0,6}".prop_map(ClickEvent::RunCommand),
                ".{0,6}".prop_map(ClickEvent::OpenUrl),
                any::<i32>().prop_map(ClickEvent::ChangePage),
                ".{0,6}".prop_map(ClickEvent::CopyToClipboard),
            ]),
            proptest::option::of(".{0,6}"),
        )
            .prop_map(|(color, shadow, bold, italic, click, insertion)| Style {
                color,
                shadow_color: shadow,
                bold,
                italic,
                click_event: click,
                insertion,
                ..Style::default()
            })
    }

    fn component() -> impl Strategy<Value = Component> {
        let leaf = (
            prop_oneof![
                ".{0,8}".prop_map(Content::Text),
                "[a-z.]{1,8}".prop_map(Content::Keybind),
                ("[a-z.]{1,8}", proptest::option::of("[a-z]{0,4}")).prop_map(|(key, fallback)| {
                    Content::Translatable {
                        key,
                        fallback,
                        with: Vec::new(),
                    }
                }),
            ],
            style(),
        )
            .prop_map(|(content, style)| Component {
                content,
                style,
                extra: Vec::new(),
            });
        leaf.prop_recursive(3, 24, 4, |inner| {
            (
                inner.clone(),
                proptest::collection::vec(inner.clone(), 0..3),
                proptest::option::of(inner),
            )
                .prop_map(|(mut c, extra, hover)| {
                    c.extra = extra;
                    if let Some(h) = hover {
                        c.style.hover_event = Some(HoverEvent::ShowText(Box::new(h)));
                    }
                    c
                })
        })
    }

    proptest! {
        #[test]
        fn json_and_nbt_round_trip(c in component()) {
            for f in [TextFormat::V769, TextFormat::V770] {
                prop_assert_eq!(&Component::from_json(&c.to_json(f)).unwrap(), &c);
                prop_assert_eq!(&Component::from_nbt(&c.to_nbt(f)).unwrap(), &c);
            }
        }

        #[test]
        fn parsers_never_panic(s in ".{0,64}") {
            let _ = parse_mini(&s);
            let _ = parse_legacy(&s);
            let _ = Component::from_json(&s);
        }

        #[test]
        fn mini_with_tag_soup_never_panics(s in "[<>/:'\"\\\\a-z#0-9!]{0,48}") {
            let c = parse_mini(&s);
            let _ = Component::from_json(&c.to_json(TextFormat::V770)).unwrap();
        }

        #[test]
        fn nbt_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..128)) {
            if let Ok(Some(tag)) = pumbo_nbt::read_network(&mut data.as_slice(), pumbo_nbt::Limits::CLIENT) {
                let _ = Component::from_nbt(&tag);
            }
        }
    }
}

#[test]
fn maximum_depth_fits_a_small_stack() {
    // The deepest accepted nesting, alternating children and hover text,
    // decodes on a 512 KiB stack even in a debug build (tokio workers have
    // 2 MiB).
    let mut v = json!("x");
    for i in 0..pumbo_text::MAX_DEPTH {
        v = if i % 2 == 0 {
            json!({"text": "", "extra": [v]})
        } else {
            json!({"text": "", "hover_event": {"action": "show_text", "value": v}})
        };
    }
    let nbt = Component::from_json_value(&v)
        .unwrap()
        .to_nbt(TextFormat::V770);
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let a = Component::from_json_value(&v).is_ok();
            let b = Component::from_nbt(&nbt).is_ok();
            a && b
        })
        .unwrap();
    assert!(handle.join().unwrap());
}

fn s(v: &str) -> Tag {
    Tag::String(v.into())
}

fn compound(entries: Vec<(&str, Tag)>) -> Tag {
    Tag::Compound(Compound(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    ))
}

fn wrapped(v: Tag) -> Tag {
    compound(vec![("", v)])
}

/// Shapes recorded from a vanilla 26.3 server (`system_chat` after
/// `/bossbar set ... value 50` and similar): primitive translation
/// arguments, wrapped in `{"": x}` when the list mixes types.
#[test]
fn vanilla_translation_arguments_round_trip() {
    let boss = compound(vec![
        ("translate", s("chat.square_brackets")),
        (
            "with",
            Tag::List(List::new(pumbo_nbt::id::STRING, vec![s("Boss")])),
        ),
        ("color", s("white")),
        ("insertion", s("pumbo:test")),
        (
            "hover_event",
            compound(vec![("action", s("show_text")), ("value", s("pumbo:test"))]),
        ),
    ]);
    let bossbar = compound(vec![
        ("translate", s("commands.bossbar.set.value.success")),
        (
            "with",
            Tag::List(List::new(
                pumbo_nbt::id::COMPOUND,
                vec![boss.clone(), wrapped(Tag::Int(50))],
            )),
        ),
    ]);
    let address = compound(vec![
        ("translate", s("x")),
        (
            "with",
            Tag::List(List::new(
                pumbo_nbt::id::COMPOUND,
                vec![boss, wrapped(Tag::Int(25565)), wrapped(s("127.0.0.1"))],
            )),
        ),
    ]);
    for tag in [bossbar.clone(), address] {
        let c = Component::from_nbt(&tag).unwrap();
        let back = c.to_nbt(TextFormat::V770);
        assert!(back.equivalent(&tag), "{back:?}\n!=\n{tag:?}");
    }
    let c = Component::from_nbt(&bossbar).unwrap();
    let Content::Translatable { with, .. } = &c.content else {
        panic!("not translatable");
    };
    assert_eq!(
        with.get(1),
        Some(&Argument::Value(pumbo_text::Raw::Nbt(Tag::Int(50))))
    );
    let Some(Argument::Component(inner)) = with.first() else {
        panic!("first argument is not a component");
    };
    assert_eq!(inner.plain_text(), "chat.square_brackets");
    let Content::Translatable {
        with: inner_with, ..
    } = &inner.content
    else {
        panic!();
    };
    assert_eq!(inner_with, &vec![Argument::from(Component::text("Boss"))]);
}

#[test]
fn json_primitive_arguments_stay_primitive() {
    let v = json!({"translate": "x", "with": [50, true, 2.5, "s", {"text": "c", "bold": true}]});
    let c = Component::from_json_value(&v).unwrap();
    assert_eq!(c.to_json_value(TextFormat::V770), v);
    assert_eq!(c.to_json_value(TextFormat::V767), v);
}
