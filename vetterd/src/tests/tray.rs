//! Tests for [`crate::tray`]. Layout convention from `AGENTS.md`.
//!
//! Everything here exercises the pure model — label lowering, tooltip
//! text, badge state, and menu shape. Publishing the item needs a
//! session bus and a `StatusNotifierWatcher`, neither of which CI
//! has, so `install` itself is covered by the manual smoke procedure
//! in `plans/LinuxApp.md` §7 step 8 instead.

use super::*;

fn summary(id: &str, verb: &str, target: &str) -> PromptSummary {
    PromptSummary {
        id: id.into(),
        command: "curl".into(),
        primary_verb: verb.into(),
        primary_target: target.into(),
        force_prompt: false,
        signals: Vec::new(),
        parsed: None,
        host_known: Vec::new(),
        peer_sid: None,
    }
}

fn card(id: &str, label: &str) -> PendingCard {
    PendingCard {
        id: id.into(),
        label: label.into(),
    }
}

// ── card_label ──────────────────────────────────────────────────────────────

#[test]
fn card_label_joins_command_verb_and_target() {
    let s = summary("01A", "GET", "https://example.test/");
    assert_eq!(card_label(&s), "curl GET https://example.test/");
}

#[test]
fn card_label_omits_an_empty_verb() {
    let s = summary("01A", "", "https://example.test/");
    assert_eq!(card_label(&s), "curl https://example.test/");
}

#[test]
fn card_label_marks_dry_run() {
    let mut s = summary("01A", "GET", "https://example.test/");
    s.force_prompt = true;
    assert!(card_label(&s).ends_with("(dry run)"), "{}", card_label(&s));
}

#[test]
fn card_label_truncates_long_targets_on_a_char_boundary() {
    let s = summary(
        "01A",
        "GET",
        &format!("https://example.test/{}", "a".repeat(200)),
    );
    let label = card_label(&s);
    assert_eq!(label.chars().count(), LABEL_MAX_CHARS);
    assert!(label.ends_with('…'), "{label}");
}

#[test]
fn card_label_truncation_does_not_split_a_multibyte_char() {
    // Every char is 3 bytes; a byte-wise cut would panic or produce
    // replacement characters.
    let s = summary("01A", "GET", &"あ".repeat(200));
    let label = card_label(&s);
    assert_eq!(label.chars().count(), LABEL_MAX_CHARS);
    assert!(label.is_char_boundary(label.len()));
}

#[test]
fn card_label_sanitizes_control_bytes_in_the_target() {
    // An RTLO override in a URL would otherwise be painted verbatim
    // into the menu a human approves from.
    let s = summary("01A", "GET", "https://evil.test/\u{202e}gnp.exe");
    let label = card_label(&s);
    assert!(
        !label.contains('\u{202e}'),
        "RTLO survived sanitisation: {label:?}"
    );
}

// ── tooltip / badge ─────────────────────────────────────────────────────────

#[test]
fn tooltip_reports_the_pending_count() {
    assert_eq!(tooltip_for(0).1, "No pending approvals");
    assert_eq!(tooltip_for(1).1, "1 request waiting for approval");
    assert_eq!(tooltip_for(4).1, "4 requests waiting for approval");
}

#[test]
fn tooltip_title_is_stable() {
    assert_eq!(tooltip_for(0).0, "Vetter");
    assert_eq!(tooltip_for(9).0, "Vetter");
}

#[test]
fn badge_asks_for_attention_only_when_something_is_pending() {
    assert_eq!(badge_for(0), Badge::Idle);
    assert_eq!(badge_for(1), Badge::Attention);
    assert_eq!(badge_for(12), Badge::Attention);
}

// ── menu model ──────────────────────────────────────────────────────────────

#[test]
fn empty_menu_still_offers_quit() {
    let menu = menu_model(&[], false);
    assert_eq!(
        menu,
        vec![
            MenuEntry::Header("No pending approvals".into()),
            MenuEntry::Separator,
            MenuEntry::Quit,
        ]
    );
}

#[test]
fn menu_lists_one_submenu_per_pending_request() {
    let cards = vec![card("01A", "curl GET a"), card("01B", "curl GET b")];
    let menu = menu_model(&cards, false);
    assert_eq!(menu[0], MenuEntry::Header("Pending: 2".into()));
    assert_eq!(
        menu[1],
        MenuEntry::Request {
            id: "01A".into(),
            label: "curl GET a".into(),
            open: false,
        }
    );
    assert_eq!(
        menu[2],
        MenuEntry::Request {
            id: "01B".into(),
            label: "curl GET b".into(),
            open: false,
        }
    );
    assert_eq!(menu[menu.len() - 1], MenuEntry::Quit);
}

#[test]
fn menu_caps_the_request_list_and_says_how_many_were_elided() {
    let cards: Vec<PendingCard> = (0..MAX_MENU_REQUESTS + 4)
        .map(|i| card(&format!("01{i:02}"), "curl GET x"))
        .collect();
    let menu = menu_model(&cards, false);

    let requests = menu
        .iter()
        .filter(|e| matches!(e, MenuEntry::Request { .. }))
        .count();
    assert_eq!(requests, MAX_MENU_REQUESTS);
    assert!(
        menu.contains(&MenuEntry::Header("…and 4 more".into())),
        "{menu:#?}"
    );
}

#[test]
fn menu_at_exactly_the_cap_has_no_elision_header() {
    let cards: Vec<PendingCard> = (0..MAX_MENU_REQUESTS)
        .map(|i| card(&format!("01{i:02}"), "curl GET x"))
        .collect();
    let menu = menu_model(&cards, false);
    assert!(
        !menu
            .iter()
            .any(|e| matches!(e, MenuEntry::Header(h) if h.contains("more"))),
        "{menu:#?}"
    );
}

// ── snapshot ────────────────────────────────────────────────────────────────

#[test]
fn snapshot_orders_cards_by_id() {
    let queue = PendingQueue::new();
    // Submit out of order; ULIDs sort chronologically so the menu
    // should come back sorted regardless of insertion order.
    let _rx_b = queue.submit(summary("01B", "GET", "https://b.test/"));
    let _rx_a = queue.submit(summary("01A", "GET", "https://a.test/"));

    let ids: Vec<String> = snapshot(&queue).into_iter().map(|c| c.id).collect();
    assert_eq!(ids, vec!["01A".to_string(), "01B".to_string()]);
}

#[test]
fn snapshot_of_an_idle_queue_is_empty() {
    let queue = PendingQueue::new();
    assert!(snapshot(&queue).is_empty());
}

// ── icons ───────────────────────────────────────────────────────────────────

#[test]
fn embedded_pixmaps_are_well_formed_argb32() {
    for icon in icon_pixmaps() {
        let expected = (icon.width * icon.height * 4) as usize;
        assert_eq!(
            icon.data.len(),
            expected,
            "{}x{} should be {expected} ARGB32 bytes",
            icon.width,
            icon.height
        );
    }
}

#[test]
fn embedded_pixmaps_are_not_fully_transparent() {
    // A blank blob would publish an invisible tray icon, which looks
    // exactly like "the tray is broken".
    for icon in icon_pixmaps() {
        let opaque = icon
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] != 0)
            .count();
        assert!(
            opaque > 0,
            "{}x{} pixmap is fully transparent",
            icon.width,
            icon.height
        );
    }
}

#[test]
fn open_window_entry_appears_only_when_a_window_exists() {
    // On a display-less box the driver stays on `PlatformDriver::None`
    // and no window is ever built, so offering "Open Vetter…" would
    // be a menu item that silently does nothing — the reason 6c
    // omitted it entirely rather than shipping it disabled.
    let cards = vec![PendingCard {
        id: "01A".into(),
        label: "curl GET https://example.com/".into(),
    }];

    let without = menu_model(&cards, false);
    assert!(
        !without.iter().any(|e| matches!(e, MenuEntry::OpenWindow)),
        "no window means no Open Vetter… entry"
    );

    let with = menu_model(&cards, true);
    assert!(
        with.iter().any(|e| matches!(e, MenuEntry::OpenWindow)),
        "a window should be reachable from the tray"
    );
}

#[test]
fn open_window_sits_above_quit() {
    // Quit stays the last item: it is destructive and users reach for
    // the bottom of a menu for it.
    let entries = menu_model(&[], true);
    let open = entries
        .iter()
        .position(|e| matches!(e, MenuEntry::OpenWindow))
        .expect("Open Vetter… present");
    let quit = entries
        .iter()
        .position(|e| matches!(e, MenuEntry::Quit))
        .expect("Quit present");
    assert!(open < quit, "Open Vetter… must precede Quit");
}

// ── Per-request Open (§6i) ──────────────────────────────────────────────────

#[test]
fn each_request_offers_open_when_a_window_exists() {
    // Without it the tray can only *decide* a request, never inspect
    // it — so a user who missed the banner is deciding blind.
    let menu = menu_model(&[card("01A", "curl GET a")], true);
    assert_eq!(
        menu[1],
        MenuEntry::Request {
            id: "01A".into(),
            label: "curl GET a".into(),
            open: true,
        }
    );
}

#[test]
fn a_display_less_daemon_offers_no_per_request_open() {
    // Same rule as the top-level "Open Vetter…" entry: there is no
    // window to raise, and a dead menu item is worse than none.
    let menu = menu_model(&[card("01A", "curl GET a")], false);
    let MenuEntry::Request { open, .. } = &menu[1] else {
        panic!("expected a request entry, got {:?}", menu[1]);
    };
    assert!(!open);
    assert!(!menu.contains(&MenuEntry::OpenWindow));
}
