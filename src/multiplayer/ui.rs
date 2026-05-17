//! bevy_ui status overlay for the multiplayer pre-connection states.
//! Lives independently of the main-menu chunky-pixel chrome so it can
//! use crisp bevy_ui text without fighting the menu's mesh-based
//! buttons. Single Node, single text child, always present (hidden
//! when `NetMode::Solo` or `Connected`).
//!
//! What it shows:
//! - `Hosting`        : "HOSTING ON x.x.x.x:port — WAITING (ESC to cancel)"
//! - `JoiningEntry`   : "ENTER HOST IP: <typed buf>_" + hint line
//! - `JoiningWait`    : "CONNECTING..."
//! - `Solo` / `Connected` : hidden

use bevy::prelude::*;

use crate::AppState;

use super::{tear_down_session, HostStatus, JoinIpEntry, NetMode, NetSession};

/// Marker on the outer overlay container Node. Lets the update
/// system find it without iterating every UI node in the world.
#[derive(Component)]
pub struct NetStatusOverlay;

/// Full-screen semi-transparent dimmer rendered BEHIND the
/// `NetStatusOverlay` card. Visible whenever the overlay is, so any
/// MP pre-connection popover (name entry, host status, IP entry,
/// connecting) reads as a true modal — the main menu beneath is
/// greyed out and clearly inactive.
#[derive(Component)]
pub struct NetStatusBackdrop;

/// Marker on the card's title text (e.g. "YOUR NAME" / "JOIN HOST").
#[derive(Component)]
pub struct NetStatusTitle;

/// Marker on the input "field" Node — used so we can toggle its
/// border colour to highlight when it's actively accepting input.
#[derive(Component)]
pub struct NetStatusInputField;

/// Marker on the text shown inside the input field (the typed
/// buffer + cursor, the LAN IP, or the connecting indicator).
#[derive(Component)]
pub struct NetStatusInputText;

/// Marker on the help / hint line under the input field.
#[derive(Component)]
pub struct NetStatusHelpText;

/// Marker on a secondary line — used for the "ESC TO CANCEL" hint
/// in hosting / joining modes. Hidden when not needed so the card
/// height shrinks to fit.
#[derive(Component)]
pub struct NetStatusSubHelpText;

/// Marker on the popover's "GO" submit button. Click does the
/// same thing as pressing Enter — gives non-keyboard players an
/// affordance for committing the name / IP. Hidden when the
/// current state has nothing to submit (Connected, JoiningWait,
/// Hosting once bound).
#[derive(Component)]
pub struct NetStatusSubmitButton;

/// Marker on the submit button's label so visibility flips with
/// the parent.
#[derive(Component)]
pub struct NetStatusSubmitButtonLabel;

/// Legacy alias kept for tests / external references; the new
/// structured layout uses [`NetStatusInputText`] for what was the
/// single status text node.
#[derive(Component)]
pub struct NetStatusText;

/// Marker on the lag-indicator outer Node. Sits in the top-right
/// corner of the window, hidden unless the connection is showing
/// signs of latency or packet loss.
#[derive(Component)]
pub struct NetLagIndicator;

/// Marker on the lag-indicator's text child.
#[derive(Component)]
pub struct NetLagIndicatorText;

/// Lag thresholds (seconds). Below `WARN_SEC` the indicator is
/// hidden; between WARN and CRIT it shows yellow "LAG"; past CRIT
/// it goes red with a stronger label. Tuned around the 50ms
/// snapshot cadence — anything past 5× that is a clear hiccup.
const NET_LAG_WARN_SEC: f32 = 0.30;
const NET_LAG_CRIT_SEC: f32 = 1.00;

/// Spawn the (initially hidden) overlay once at Startup. We don't gate
/// it on `MainMenu` enter because the menu state can be re-entered
/// many times and respawning the overlay would race the menu's own
/// chrome lifecycle.
///
/// Layout: a centered card with a title row, an "input field" box
/// (rounded, bordered) containing the live value, and two help
/// lines. `update_overlay` flips visibility + text + border colour
/// each frame based on the current `NetMode`.
pub fn setup_overlay(mut commands: Commands, font: Res<crate::fonts::PixelFont>) {
    use crate::ui_kit::theme;

    // Full-screen dimmer behind the popover. Lower ZIndex than the
    // overlay card so the card paints on top. `update_overlay` flips
    // both nodes' visibility together so the modal feel is consistent.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(0.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            bottom: Val::Px(0.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        Visibility::Hidden,
        ZIndex(499),
        NetStatusBackdrop,
    ));

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(12.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            Visibility::Hidden,
            // High ZIndex so the card sits above the menu chrome
            // sprite AND the backdrop dimmer.
            ZIndex(500),
            NetStatusOverlay,
        ))
        .with_children(|root| {
            // Card container.
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(theme::GAP_MD),
                    padding: UiRect::axes(Val::Px(theme::PAD_LG), Val::Px(theme::PAD_LG)),
                    border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                    min_width: Val::Px(260.0),
                    ..default()
                },
                BackgroundColor(theme::SURFACE),
                BorderColor(theme::ACCENT),
                BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
            ))
            .with_children(|card| {
                // Title.
                card.spawn((
                    crate::ui_kit::pixel_label(
                        &font, "",
                        theme::FONT_LG,
                        theme::ACCENT,
                    ),
                    TextShadow {
                        offset: Vec2::splat(1.0),
                        color: Color::srgba(0.0, 0.0, 0.0, 0.85),
                    },
                    NetStatusTitle,
                ));

                // Input field box — the typed buffer / IP / status line
                // lives here. Border colour switches based on focus.
                card.spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        padding: UiRect::axes(Val::Px(theme::PAD_MD), Val::Px(theme::PAD_SM)),
                        border: UiRect::all(Val::Px(theme::CHUNKY_BORDER_W)),
                        min_width: Val::Px(200.0),
                        min_height: Val::Px(28.0),
                        ..default()
                    },
                    BackgroundColor(theme::SURFACE_RAISED),
                    BorderColor(theme::ON_SURFACE_DIM),
                    BorderRadius::all(Val::Px(theme::CHUNKY_RADIUS)),
                    NetStatusInputField,
                ))
                .with_children(|field| {
                    field.spawn((
                        crate::ui_kit::pixel_label(
                            &font, "",
                            theme::FONT_LG,
                            theme::ON_SURFACE,
                        ),
                        NetStatusInputText,
                        // Keep the legacy marker for back-compat with
                        // any test that still queries by name.
                        NetStatusText,
                    ));
                });

                // Primary help line.
                card.spawn((
                    crate::ui_kit::pixel_label(
                        &font, "",
                        theme::FONT_MD,
                        theme::ON_SURFACE_DIM,
                    ),
                    NetStatusHelpText,
                ));

                // Secondary help line — "ESC TO CANCEL" etc.
                card.spawn((
                    crate::ui_kit::pixel_label(
                        &font, "",
                        theme::FONT_SM,
                        theme::ON_SURFACE_DIM,
                    ),
                    NetStatusSubHelpText,
                ));

                // Submit button — same affordance as pressing Enter.
                // Lets non-keyboard players commit the name / IP.
                // `update_overlay` hides it for states that have
                // nothing to confirm.
                card.spawn((
                    crate::ui_kit::button(theme::SURFACE_RAISED),
                    NetStatusSubmitButton,
                ))
                .with_children(|b| {
                    b.spawn((
                        crate::ui_kit::pixel_label(
                            &font, "GO",
                            theme::FONT_LG,
                            theme::ON_SURFACE,
                        ),
                        NetStatusSubmitButtonLabel,
                    ));
                });
            });
        });
}

/// Event fired when the player commits the current popover stage
/// — either by pressing Enter inside a text field or by clicking
/// the GO button. Consumed by the same handlers that previously
/// only saw Enter (`capture_name_keys` for naming states,
/// `capture_join_ip_keys` for the IP entry).
#[derive(Event)]
pub struct NetOverlaySubmit;

/// Translate a click on the GO submit button into a
/// [`NetOverlaySubmit`] event so the existing Enter handlers in
/// `capture_name_keys` / `capture_join_ip_keys` pick it up
/// without per-stage button wiring.
pub fn handle_submit_button_click(
    interactions: Query<&Interaction, (Changed<Interaction>, With<NetStatusSubmitButton>)>,
    mut writer: EventWriter<NetOverlaySubmit>,
) {
    for interaction in &interactions {
        if matches!(*interaction, Interaction::Pressed) {
            writer.write(NetOverlaySubmit);
        }
    }
}

/// Per-frame: update overlay visibility + text + focus colour from
/// `NetMode`. Cheap — sets text only when the value actually changes
/// so the renderer's diff stays small.
pub fn update_overlay(
    mode: Res<NetMode>,
    host: Res<HostStatus>,
    join: Res<JoinIpEntry>,
    name: Res<super::LocalPlayerName>,
    state: Res<bevy::prelude::State<crate::AppState>>,
    mut overlay_q: Query<&mut Visibility, (With<NetStatusOverlay>, Without<NetStatusBackdrop>, Without<NetStatusSubmitButton>)>,
    mut backdrop_q: Query<&mut Visibility, (With<NetStatusBackdrop>, Without<NetStatusOverlay>, Without<NetStatusSubmitButton>)>,
    mut submit_q: Query<&mut Visibility, (With<NetStatusSubmitButton>, Without<NetStatusOverlay>, Without<NetStatusBackdrop>)>,
    mut field_q: Query<&mut BorderColor, With<NetStatusInputField>>,
    mut title_q: Query<&mut Text, (With<NetStatusTitle>, Without<NetStatusInputText>, Without<NetStatusHelpText>, Without<NetStatusSubHelpText>)>,
    mut input_q: Query<&mut Text, (With<NetStatusInputText>, Without<NetStatusTitle>, Without<NetStatusHelpText>, Without<NetStatusSubHelpText>)>,
    mut help_q:  Query<&mut Text, (With<NetStatusHelpText>,  Without<NetStatusTitle>, Without<NetStatusInputText>, Without<NetStatusSubHelpText>)>,
    mut sub_q:   Query<&mut Text, (With<NetStatusSubHelpText>, Without<NetStatusTitle>, Without<NetStatusInputText>, Without<NetStatusHelpText>)>,
) {
    use crate::ui_kit::theme;

    let on_main_menu = *state.get() == crate::AppState::MainMenu;

    // Per-mode card content. `focused` flips the input field's
    // border colour to the accent so the player can tell at a glance
    // which mode is active.
    let (visible, title, input, help, sub, focused) = match *mode {
        // Solo on the main menu: nothing — the name prompt only
        // appears once the player commits to HOST or JOIN.
        NetMode::Solo | NetMode::Connected => (
            false, "", String::new(), String::new(), String::new(), false,
        ),
        // Same popover layout for the two naming states — title +
        // sub-help vary so the player knows which flow they're in.
        NetMode::NamingForHost => (
            true,
            "HOST: YOUR NAME",
            format!("{}_", name.0),
            "TYPE TO EDIT — ENTER TO HOST".to_string(),
            "ESC TO CANCEL".to_string(),
            true,
        ),
        NetMode::NamingForJoin => (
            true,
            "JOIN: YOUR NAME",
            format!("{}_", name.0),
            "TYPE TO EDIT — ENTER TO CONTINUE".to_string(),
            "ESC TO CANCEL".to_string(),
            true,
        ),
        NetMode::Hosting => (
            true,
            "HOSTING",
            format!("{}:{}", host.lan_ip, host.port),
            "SHARE THIS WITH YOUR PARTNER".to_string(),
            "ESC TO CANCEL".to_string(),
            false,
        ),
        NetMode::JoiningEntry => {
            let err = join.last_error.as_deref().unwrap_or("");
            let help_line = if err.is_empty() {
                "DIGITS . AND : — ENTER TO CONNECT".to_string()
            } else {
                err.to_string()
            };
            (
                true,
                "JOIN HOST",
                format!("{}_", join.buf),
                help_line,
                "ESC TO CANCEL".to_string(),
                true,
            )
        }
        NetMode::JoiningWait => (
            true,
            "CONNECTING",
            "...".to_string(),
            "WAITING FOR HOST".to_string(),
            "ESC TO CANCEL".to_string(),
            false,
        ),
    };
    // Gate the entire popover on MainMenu state. Once the player
    // transitions to Lobby (after host bind / welcome receipt) the
    // lobby screen owns its own UI for showing the LAN IP / peer
    // roster, so the floating popover would be redundant chrome.
    let visible = visible && on_main_menu;

    if let Ok(mut v) = overlay_q.single_mut() {
        let want_vis = if visible { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want_vis { *v = want_vis; }
    }
    if let Ok(mut v) = backdrop_q.single_mut() {
        let want_vis = if visible { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want_vis { *v = want_vis; }
    }
    // Submit button visible only when there's something to confirm —
    // naming / IP entry. Hidden during Hosting (already bound) and
    // JoiningWait (already sent; waiting for handshake).
    let has_submit = matches!(
        *mode,
        NetMode::NamingForHost | NetMode::NamingForJoin | NetMode::JoiningEntry,
    ) && visible;
    if let Ok(mut v) = submit_q.single_mut() {
        let want_vis = if has_submit { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want_vis { *v = want_vis; }
    }

    // Focus colour: accent when the field is actively editable,
    // dim otherwise.
    if let Ok(mut bc) = field_q.single_mut() {
        let want = if focused { theme::ACCENT } else { theme::ON_SURFACE_DIM };
        if bc.0 != want { bc.0 = want; }
    }

    // Update text labels only on change. Diff-on-write keeps the
    // bevy_ui change-detection small for every other system that
    // reads `Text`.
    if let Ok(mut t) = title_q.single_mut() {
        if t.0 != title { t.0 = title.to_string(); }
    }
    if let Ok(mut t) = input_q.single_mut() {
        if t.0 != input { t.0 = input; }
    }
    if let Ok(mut t) = help_q.single_mut() {
        if t.0 != help { t.0 = help; }
    }
    if let Ok(mut t) = sub_q.single_mut() {
        if t.0 != sub { t.0 = sub; }
    }
}

/// Spawn the (initially hidden) lag indicator at startup. Pinned to
/// the top-right; visibility + colour driven by `update_lag_indicator`
/// based on the staleness of `NetSession.last_seen` entries.
pub fn setup_lag_indicator(mut commands: Commands, font: Res<crate::fonts::PixelFont>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                right: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(6.0), Val::Px(3.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.65)),
            Visibility::Hidden,
            ZIndex(600),
            NetLagIndicator,
        ))
        .with_children(|p| {
            p.spawn((
                crate::ui_kit::pixel_label(
                    &font,
                    "",
                    14.0,
                    Color::srgb(1.0, 0.85, 0.30),
                ),
                NetLagIndicatorText,
            ));
        });
}

/// Per-frame: update the lag indicator's visibility + text + colour
/// based on how stale `NetSession.last_seen` entries are. Cheap —
/// short-circuits on Solo / no-session / not-connected.
///
/// On host: shows worst-case lag across ALL peers.
/// On client: shows lag for the host (id 0) — that's the connection
/// that matters for gameplay.
pub fn update_lag_indicator(
    mode: Res<super::NetMode>,
    session: Option<Res<NetSession>>,
    mut overlay_q: Query<&mut Visibility, With<NetLagIndicator>>,
    mut text_q: Query<(&mut Text, &mut TextColor), With<NetLagIndicatorText>>,
) {
    let want = compute_lag_label(mode.as_ref(), session.as_deref());
    if let Ok(mut v) = overlay_q.single_mut() {
        let target_vis = if want.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        if *v != target_vis { *v = target_vis; }
    }
    if let Ok((mut t, mut col)) = text_q.single_mut() {
        if let Some((label, colour)) = want {
            if t.0 != label { t.0 = label; }
            if col.0 != colour { col.0 = colour; }
        }
    }
}

/// Pure helper: given the current net mode + session, returns
/// `Some((label, color))` if the lag indicator should be visible,
/// or `None` to hide it. Extracted for testability.
pub fn compute_lag_label(
    mode: &super::NetMode,
    session: Option<&NetSession>,
) -> Option<(String, Color)> {
    let session = session?;
    if !matches!(mode, super::NetMode::Connected) { return None; }
    if !session.welcomed { return None; }

    let now = std::time::Instant::now();
    // Host: worst lag across every peer. Client: lag from host (id 0).
    let relevant_age = if session.is_host {
        session.last_seen
            .iter()
            .map(|(_, &t)| now.duration_since(t).as_secs_f32())
            .fold(f32::NAN, f32::max)
    } else {
        session.last_seen
            .get(&0)
            .map(|&t| now.duration_since(t).as_secs_f32())
            .unwrap_or(f32::NAN)
    };

    if !relevant_age.is_finite() { return None; }
    if relevant_age < NET_LAG_WARN_SEC { return None; }

    if relevant_age >= NET_LAG_CRIT_SEC {
        let label = format!("HIGH LAG ({}MS)", (relevant_age * 1000.0) as u32);
        Some((label, Color::srgb(1.0, 0.40, 0.40)))
    } else {
        let label = format!("LAG ({}MS)", (relevant_age * 1000.0) as u32);
        Some((label, Color::srgb(1.0, 0.85, 0.30)))
    }
}

#[cfg(test)]
mod lag_tests {
    use super::*;
    use super::super::{NetMode, NetSession};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    fn fake_session(is_host: bool, last_seen: HashMap<u8, Instant>) -> NetSession {
        NetSession {
            sock: super::super::net::bind_socket(None).expect("bind"),
            my_id: if is_host { 0 } else { 1 },
            peers: HashMap::new(),
            next_peer_id: if is_host { 1 } else { 0 },
            welcomed: true,
            is_host,
            last_seen,
        }
    }

    #[test]
    fn hidden_when_solo() {
        let s = fake_session(false, HashMap::new());
        assert!(compute_lag_label(&NetMode::Solo, Some(&s)).is_none());
    }

    #[test]
    fn hidden_when_no_session() {
        assert!(compute_lag_label(&NetMode::Connected, None).is_none());
    }

    #[test]
    fn hidden_when_fresh() {
        let now = Instant::now();
        let mut seen = HashMap::new();
        seen.insert(0, now);
        let s = fake_session(false, seen);
        assert!(compute_lag_label(&NetMode::Connected, Some(&s)).is_none(),
            "fresh packets should not show the indicator");
    }

    #[test]
    fn shows_warn_band_when_lag_exceeds_threshold() {
        let mut seen = HashMap::new();
        seen.insert(0, Instant::now() - Duration::from_millis(500));
        let s = fake_session(false, seen);
        let (label, _color) = compute_lag_label(&NetMode::Connected, Some(&s))
            .expect("500ms stale → visible");
        assert!(label.starts_with("LAG"), "label should be the warn label, got {label}");
    }

    #[test]
    fn shows_crit_band_when_lag_exceeds_crit_threshold() {
        let mut seen = HashMap::new();
        seen.insert(0, Instant::now() - Duration::from_millis(1500));
        let s = fake_session(false, seen);
        let (label, _color) = compute_lag_label(&NetMode::Connected, Some(&s))
            .expect("1500ms stale → visible");
        assert!(label.contains("HIGH"),
            "label should be the crit label, got {label}");
    }

    #[test]
    fn host_shows_worst_peer_lag() {
        let now = Instant::now();
        let mut seen = HashMap::new();
        seen.insert(1, now - Duration::from_millis(50));   // fresh
        seen.insert(2, now - Duration::from_millis(700));  // warn band
        let s = fake_session(true, seen);
        let (label, _) = compute_lag_label(&NetMode::Connected, Some(&s))
            .expect("700ms worst → visible");
        // Should be in the WARN band reading the worst peer's age.
        assert!(label.starts_with("LAG"), "host shows worst peer's lag, got {label}");
    }
}

/// ESC handler for the `Hosting` and `JoiningWait` states. (The
/// `JoiningEntry` ESC is handled inside `capture_join_ip_keys`
/// because that system already owns the entry buf.) Tears the
/// session down so the next attempt starts clean.
pub fn cancel_on_esc(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut mode: ResMut<NetMode>,
    session: Option<Res<NetSession>>,
    state: Res<State<AppState>>,
) {
    // Only relevant on the MainMenu — once we're in Playing the
    // gameplay ESC (pause) takes priority.
    if *state.get() != AppState::MainMenu { return; }
    if !keys.just_pressed(KeyCode::Escape) { return; }
    if !matches!(*mode, NetMode::Hosting | NetMode::JoiningWait) { return; }
    tear_down_session(&mut commands, &mut mode, session.as_deref());
    bevy::log::info!("multiplayer: cancelled by ESC");
}
