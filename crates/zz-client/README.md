# zz-client

Bevy **0.19** first-person client skeleton for ZombieZap. Native window for
dev; Wasm + WebGL2 via Trunk for the browser.

## Native

```bash
# from workspace root
cargo run -p zz-client
```

Controls: **click** to lock the pointer, **WASD** move, **Space / Ctrl** up/down,
**Shift** sprint, **Esc** release cursor. FPS is shown in the top-left and in
the window title.

## Browser (Trunk)

Requires the `wasm32-unknown-unknown` target (already listed in
`rust-toolchain.toml`) and Trunk:

```bash
cargo install trunk --locked
rustup target add wasm32-unknown-unknown   # if missing
cd web
trunk serve          # http://127.0.0.1:8080
# or
trunk build          # emits web/dist/ (wasm + JS glue)
```

### Smaller release wasm (SHIP path)

Workspace root defines `[profile.wasm-release]` (`opt-level = "z"`, fat LTO,
`strip`, `panic = "abort"`). **This is the documented ship path** (also wired
via Trunk.toml when using plain `trunk build --release`):

```bash
# from repo root (also runs gzip/brotli siblings for zz-server):
scripts/ship-web.sh
# or:
cd web && trunk build --release
```

**M18 ship size** (binaryen `wasm-opt -Oz --strip-debug --strip-producers`):
**22.20 MiB** `_bg.wasm` (23_280_475 bytes raw / 7.11 MiB gzip / **4.87 MiB
brotli**). Pre-M18 (M15 pipeline) was 24.65 MB / 8.22 MB gz / 5.80 MB br.
Cold load: loading card shows immediately; hide only after stable canvas
frames. Pre-M15 unoptimized bundles were ~28 MiB with a black-page first paint.

Requires `wasm-opt` (binaryen) on PATH for `data-wasm-opt="z"`. Debug
`trunk build` (no `--release`) is enough for iteration.

The HTML canvas id is `zz-canvas`. Bevy’s primary `Window` is configured with
`canvas: Some("#zz-canvas")`, `fit_canvas_to_parent: true`, and
`prevent_default_event_handling: true`.

## Feature list (Bevy 0.19)

Curated in this crate’s `Cargo.toml` (workspace pins `bevy = "0.19"` with
`default-features = false`):

| Area | Features |
|------|----------|
| App core | `std`, `async_executor`, `bevy_asset`, `bevy_log`, `bevy_state` — **no** `reflect_auto_register` |
| Native-only | `multi_threaded`, `x11`, `wayland` (target-gated; wasm is single-threaded) |
| Window / platform | `bevy_window`, `bevy_winit`, `webgl2`, `default_font` |
| Render / PBR | `bevy_render`, `bevy_core_pipeline`, `bevy_pbr`, `bevy_light`, `bevy_camera`, `bevy_mesh`, `bevy_material`, `bevy_image`, `bevy_shader`, `bevy_color` — **no** `tonemapping_luts` / `ktx2` (LUT-free `SomewhatBoringDisplayTransform` on both cameras) |
| UI / text | `bevy_text`, `bevy_ui`, `bevy_ui_render` |
| Audio | `bevy_audio` (no `wav`/`vorbis`/… — procedural only) |
| bevy_egui | `default_fonts`, `render`, `bevy_ui`, `open_url` only (no clipboard/picking) |

Explicitly **not** enabled: `bevy_gltf`, `bevy_animation`, file-format audio
features (`wav`, `vorbis`, `mp3`, …), `scene`, `webgpu` (browser target is
WebGL2 only), `tonemapping_luts`/`ktx2` (M18), `reflect_auto_register` (M18).

Note: `bevy_egui` still forces `bevy_image/zstd_rust` and wasm `image/png`
internally — those two cannot be fully dropped while egui is present.

## Bevy 0.19 API notes (vs 0.14–0.16 era)

Facts that surprised us while wiring this skeleton — useful for netcode/render
work next:

1. **Cursor grab is a component, not `Window` field.**  
   Use `Single<&mut CursorOptions>` and set `grab_mode` / `visible`.  
   There is no `window.cursor.grab_mode` anymore.  
   `CursorGrabMode::{None, Locked, Confined}` still exist under `bevy::window`.

2. **No more `*Bundle` types for cameras/meshes/lights.**  
   Spawn tuples: `(Camera3d::default(), Transform::…)`,  
   `(Mesh3d(handle), MeshMaterial3d(handle), Transform::…)`,  
   `(DirectionalLight { .. }, Transform::…)`.

3. **UI text is a component.**  
   `Text::new("…")` + `TextFont` + `TextColor` + `Node { top: px(10), … }`.  
   The `px()` helper is in prelude. No `TextBundle` / `Style` / `Val::Px`.

4. **Mouse motion resource.**  
   Prefer `Res<AccumulatedMouseMotion>` (delta since last update). Do **not**
   multiply mouse deltas by `delta_time` (already frame-integrated).

5. **Feature graph is profile-based.**  
   Top-level `"3d"` / `"ui"` pull scene, picking, gltf, etc. For a thin client
   enable leaf features (`bevy_pbr`, `bevy_ui_render`, `webgl2`, …) instead of
   the big profiles. There is no feature literally named `"state"` —
   use `bevy_state`.

6. **`tonemapping_luts` is required** for the default `TonyMcMapface` tonemap
   (and for AgX / BlenderFilmic). Without the feature, set an explicit
   LUT-free mode on every 3D camera — we use
   `Tonemapping::SomewhatBoringDisplayTransform` (M18). Leaving the default
   without LUTs can produce a pink / wrong-looking frame.

7. **Web canvas fields on `Window` (still):**  
   `canvas: Option<String>`, `fit_canvas_to_parent: bool`,
   `prevent_default_event_handling: bool`. Selector form: `"#zz-canvas"`.

8. **Diagnostics FPS path is a constant:**  
   `FrameTimeDiagnosticsPlugin::FPS` → `DiagnosticsStore::get` →
   `Diagnostic::smoothed()`. Plugin lives in `bevy::diagnostic` (always on
   the umbrella crate; not a separate Cargo feature).

9. **Optional built-in freecam:** `bevy_camera_controller` + `free_camera`
   (`FreeCamera` / `FreeCameraPlugin`). Defaults use RMB hold / `M` toggle,
   not click+Esc — this skeleton implements a small custom fly cam instead.

10. **Schedule names** (`Startup`, `Update`, `FixedUpdate`, …) are familiar;
    free-camera internals also use `RunFixedMainLoop` /
    `RunFixedMainLoopSystems` if you adopt the stock controller later.

11. **`FontSize` is an enum, not `f32`.**  
    `TextFont { font_size: FontSize::Px(18.0), .. }` — bare floats no longer
    compile.

12. **Shadow flag renamed.**  
    Lights use `shadow_maps_enabled` (not `shadows_enabled`).

13. **LUT stack needs `ktx2` + a zstd backend** if you re-enable
    `tonemapping_luts`. M18 drops the whole stack in favour of a LUT-free
    tonemapper. Residual `zstd_rust` / wasm `png` come from `bevy_egui`, not
    from our Bevy feature list.

14. **`despawn()` is recursive for children.**  
    There is no `despawn_recursive()` in 0.19 — `EntityCommands::despawn()`
    already despawns `Children` (and other relationship targets configured to
    cascade). Use `try_despawn()` when missing entities should not warn.

15. **Hand-made `Image` sampler:** set `image.sampler = ImageSampler::nearest()`
    (or `ImageSampler::Descriptor(ImageSamplerDescriptor::nearest())`) after
    `Image::new(...)`. Defaults come from `ImagePlugin` (`ImageSampler::Default`).

16. **Procedural mesh construction:**  
    `Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())`
    + `with_inserted_attribute(Mesh::ATTRIBUTE_{POSITION,NORMAL,UV_0}, …)`
    + `with_inserted_indices(Indices::U32(…))`. `Indices` / `PrimitiveTopology`
    live under `bevy::mesh` (not always in the umbrella prelude).

17. **Ambient light is a resource, not only a component.**  
    `GlobalAmbientLight` (resource, default brightness 80) lights the scene;
    optional `AmbientLight` component on a `Camera` overrides it. Map render
    tunes the global ambient on rebuild.

18. **Image GPU types:** `Extent3d` / `TextureDimension` / `TextureFormat` are
    reachable via `bevy::render::render_resource` (or re-exports on
    `bevy::image` internals). Prefer `TextureFormat::Rgba8UnormSrgb` for
    hand-filled RGBA buffers.

19. **bevy_egui 0.41 multipass (menu + lobby):** add `EguiPlugin::default()` and
    schedule UI systems on `EguiPrimaryContextPass` (not bare `Update`). Context
    access is fallible: `contexts.ctx_mut()?` and the system returns Bevy
    `Result`. Dark style uses `ctx.set_theme(Theme::Dark)` +
    `ctx.style_mut_of(Theme::Dark, …)` (the old `ctx.style()` /
    `set_style` pair is gone). Clipboard copy for invite links:
    `ui.ctx().copy_text(url)` → egui `OutputCommand::CopyText` → bevy_egui
    `process_output_system` → `EguiClipboard` (native arboard + wasm
    `navigator.clipboard`; works on both when the page is a secure context).
    The overlay paints `seams::LobbyView` and pushes `seams::UiIntent` only —
    never mutates `Session` or talks to `NetClient`.

20. **Procedural audio (zero assets):** enable leaf feature `bevy_audio` only —
    not the `audio` profile (that also pulls `vorbis`). Custom one-shot clips
    implement `Decodable` + `Source` (rodio trait re-exported by `bevy::audio`)
    and register with `app.add_audio_source::<T>()`. Bake mono `f32` samples
    into an `Asset` once at startup; play with `AudioPlayer(handle)` +
    `PlaybackSettings::DESPAWN.with_volume(Volume::Linear(…))`.  
    `Source::channels` / `sample_rate` return `NonZeroU16` / `NonZeroU32` in
    0.19 (the example still aliases these as `ChannelCount` / `SampleRate`).
    `Volume` is an enum (`Linear` / `Decibels`), not a bare `f32`.

21. **SFX seam:** `seams::SfxQueue` is drained solely by `audio.rs` every frame.
    Concurrent voices are capped (~8); lowest priority (remote `Shoot`) is
    skipped first when saturated. Synthesis is pure math + xorshift noise —
    no `rand`, no wav files.

22. **`FocusPolicy` is under `bevy::ui`, not always in the umbrella prelude.**  
    Use `bevy::ui::FocusPolicy::Pass` for non-interactive full-screen overlays
    (damage vignette, paused tint) so clicks fall through to menus/buttons.

23. **Per-entity transparent FX need unique `StandardMaterial` handles.**  
    Mutating a shared material’s alpha fades every concurrent tracer/boom.
    Clone via `materials.add(...)` per spawn for independent lifetimes.

24. **`Button` requires `Interaction` (via `#[require]`) — edge-detect clicks with**  
    `Changed<Interaction>` + `Interaction::Pressed` and a local “was pressed”
    flag so held clicks don’t spam `UiIntent`s every frame.

25. **`RenderTarget` is a component, not a `Camera` field.**  
    In 0.19, `Camera` requires a `RenderTarget` component (default:
    primary window). Offscreen passes spawn  
    `RenderTarget::Image(image_handle.into())` as a sibling of `Camera3d` /
    `Camera { .. }` — there is no `Camera.target` field.  
    `From<Handle<Image>> for ImageRenderTarget` sets `scale_factor: 1.0`.

26. **Offscreen target images:**  
    `Image::new_target_texture(w, h, format, view_format)` (in `bevy_image`)
    zeros the buffer and sets `TEXTURE_BINDING | COPY_DST | RENDER_ATTACHMENT`.
    Match Bevy’s `render_to_texture` example: storage
    `TextureFormat::Rgba8Unorm` + view `Some(Rgba8UnormSrgb)` for SDR PBR.
    Then set `image.sampler = ImageSampler::nearest()` for chunky upscale
    (item 15).

27. **Distance fog is a camera component:**  
    `bevy::pbr::DistanceFog` + `FogFalloff::Linear { start, end }` (or
    Exponential / Atmospheric). Color should match sky / `ClearColor` so the
    horizon dissolves instead of cutting to a different hue. Insert/replace
    on the 3D camera when the map (env) changes.

28. **Per-camera MSAA:**  
    `Msaa` (`bevy::render::view::Msaa`, also a component) can be set per
    camera entity — use `Msaa::Off` on the low-res 3D camera so nearest
    upscale stays crisp. Default is `Sample4`.

29. **bevy_egui primary context must not land on the retro camera:**  
    Set `EguiGlobalSettings::auto_create_primary_context = false` in
    PreStartup, then put `PrimaryEguiContext` on the native-res present
    camera (with `IsDefaultUiCamera`) so menus/HUD stay full resolution.
    Present the 480×270 frame via a full-window `ImageNode` +
    `NodeImageMode::Stretch` letterboxed under HUD (`GlobalZIndex` negative).

## M6a — browser platform seam + Trunk release

`platform.rs` is the only place that branches on `cfg(target_arch = "wasm32")`
for connect URL / invite join / name persistence:

| API | Native | Wasm |
|-----|--------|------|
| `server_url()` | `ZZ_SERVER` or `ws://127.0.0.1:8080/ws` | same origin: `wss://` if page is https, else `ws://`, path `/ws` |
| `join_code_from_url()` | `ZZ_JOIN` | `?join=CODE` on `location.search` |
| `initial_name()` | `ZZ_NAME` or `"survivor"` | `localStorage["zz_name"]` or `"survivor"` |
| `persist_name(name)` | no-op | writes `localStorage["zz_name"]` |

`LobbyView.join_prefill` carries the invite code; `lobby_ui` copies it into the
join field **once** so later typing is not overwritten.

### CI / release Trunk invocation

```bash
# from workspace root (trunk 0.21+)
cd web
trunk build --release
# → web/dist/  (uses [profile.wasm-release] via Trunk.toml cargo_profile,
#               and binaryen -Oz via data-wasm-opt="z" on the rust link tag)
```

Override without config: `trunk build --release --cargo-profile wasm-release`.

### Wasm-specific API notes (M6a)

30. **Same-origin WebSocket only in the browser.** There is no hardcoded host:
    `window.location.protocol` / `.host` drive the scheme. A static host that
    is not the API origin needs a reverse proxy (serve `dist/` and `/ws` from
    one host) or a future config seam — do not special-case hosts in client code.

31. **`web-sys` features are minimal:** `Window`, `Location`, `Storage`. No
    `Url` / `UrlSearchParams` — query parsing is a tiny split on
    `location.search` so the wasm dep graph stays small.

32. **M19 HTML-first start screen (instant boot).** `#zz-start` is in the
    static markup *above* the wasm module: CALLSIGN / LOBBY CODE / HOST /
    JOIN are interactive immediately while Bevy boots. Typing works; HOST/
    JOIN queue into `window.__zzBoot` and fire once after
    `seams::HtmlBoot` engine-ready + Session::Menu (`apply_boot_handoff` —
    no double-send). Trunk `data-initializer="boot-init.mjs"` marks
    fetch/instantiate; Rust logs first Update as `first_frame_ms`. Console
    one-liner: `[zz boot] fetch=…ms instantiate=…ms startup=…ms
    first_frame=…ms loader_hide=…ms`. **KHR_parallel_shader_compile:**
    wgpu 29 GLES does **not** use it (sync `compile_shader` +
    `get_shader_compile_status` only; no feature flag) — unblock is
    staggered pipeline warmup (camera-only Startup, skeleton cubes over
    frames 2–8, match FX/rigs during menu).
    **M19b:** never use `std::time::Instant` on wasm32-unknown-unknown in
    the boot path — it panics ("time not implemented") and with
    `panic=abort` + fat LTO the engine is DCE'd (~5.8 MB ship wasm, no
    `[zz boot]` line). Use `platform::BootStamp`. Ship canaries in
    `scripts/ship-web.sh` (size ≥ 15 MB, `zz-boot-entry-v1`,
    `__wbindgen_start`).

33. **`data-wasm-opt="z"` needs binaryen (`wasm-opt`) on PATH.** Without it,
    Trunk fails the release link step — install via package manager or
    temporarily set `data-wasm-opt="0"` for local iteration. Rust ≥1.82
    emits bulk-memory / nontrapping float-to-int ops; pass
    `data-wasm-opt-params="--enable-bulk-memory --enable-nontrapping-float-to-int"`
    or wasm-opt validation fails with `memory.copy` / `memory.fill` errors.

34. **`NetClient` needs `unsafe impl Send + Sync` on wasm32.** ewebsock’s
    browser `WsSender` wraps `Rc<WebSocket>` (`!Send`/`!Sync`), but Bevy’s
    `Resource` trait still demands both bounds. The client is single-threaded
    on wasm (main browser thread only), so the impl is sound in practice.
    See `net.rs`.

35. **WebGL2 forbids texture view-format reinterpretation.** A render-target
    `Image` created with linear storage + an sRGB *view* format
    (`new_target_texture(w, h, Rgba8Unorm, Some(Rgba8UnormSrgb))`) works on
    native Metal but on the wgpu GL backend silently kills the entire render
    world: transparent canvas, no camera ever presents, endless
    "CommandQueue has un-applied commands" console spam. Use plain
    `Rgba8UnormSrgb` storage with `None` view format for the retro target.

36. **Browser-automation verification quirks** (Claude/CDP browser pane):
    synthesized DOM `KeyboardEvent`s and CDP key taps never reach winit —
    only real mouse input works. egui buttons may eat a same-frame
    click; a 1 px `left_click_drag` press-releases across frames and lands
    reliably. Keyboard turn keys (Q/X, `game.rs`) exist partly so future
    touch-mode automation (`?touch=1`, item 3) can drive the player with
    clicks alone.

37. **Procedural humanoid rigs (M9 + M15 LOD):** articulated cuboids hang from
    empty joint-pivot entities (`models.rs`). One shared `Cuboid` mesh + a
    small material bank (3 zombie kinds, 5 slot colours, 1 emissive eye, 1
    gun) — never per-zombie unique mesh/material at spawn (ShotAnte draw-call
    warning). **Exception (note 23):** hit-flash briefly swaps body parts onto
    unique white materials, then restores the shared handles. Animation LOD:
    full limbs ≤ 40 m (capped to nearest 40 zombies), bob 40–80 m, single
    shared-mesh impostor cuboid beyond 80 m. Walk cycles are client-only from
    interpolated velocity; phase offset is a pure hash of entity id. Death
    crumples are short-lived FX entities — do not delay `net_poll` despawn.

38. **Viewmodel parenting:** first-person rifle is a `ChildOf` the 3D
    `Camera3d` entity (camera-local bottom-right). Own `Shot` events kick
    recoil via a side channel (`ViewmodelKick`) so hud can still drain
    `FxQueue` for tracers without racing the viewmodel. Muzzle-flash uses a
    unique `StandardMaterial` handle (item 23) with alpha + visibility fade.

38b. **Muzzle visual origin (M17):** own tracers start at the viewmodel muzzle
    tip (`models::VIEWMODEL_MUZZLE_LOCAL` on the rifle child — same point as
    the flash quad), transformed with `muzzle_world_from_camera` +
    `viewmodel_muzzle_camera_offset` (rest pose under the camera). Remote
    shots use `remote_muzzle_from_feet` (eye − 0.12 m, ShotAnte legacy) until
    the TP rig carries a gun. Impact sparks spawn at the shot endpoint with a
    unique material per spawn (note 23) and ~0.2 s TTL. Server hitscan stays
    eye-based — this is cosmetic only.

39. **Touch mode (M10):** `platform::is_touch_mode()` — wasm uses
    `navigator.maxTouchPoints` with `?touch=1` / `?touch=0` override; native
    uses `ZZ_TOUCH=1`. `touch::TouchPlugin` draws stick + FIRE/JUMP/NADE on
    the present camera (native-res bevy_ui, `GlobalZIndex(50)`). Left half
    (~45%) owns the stick; right half drag is aim only (taps do not fire).
    Mouse LMB is treated as a single pointer so desktop `?touch=1` is
    automatable. Intent lands in `TouchIntent` and is OR'd into the same
    30 Hz `PlayerInput` path in `game::fps_controller` (send cadence
    untouched). Stick dead zone 0.35; aim sens `0.0025 * 2.0` rad/px.

40. **Soft-keyboard bridge:** egui `TextEdit` never summons mobile keyboards.
    `web/index.html` has `#zz-name-input` / `#zz-code-input` overlays;
    `platform::set_touch_text_overlays` + DOM poll (`html_name_value` /
    `html_code_value`) bridge into `UiIntent::SetName` / join draft while
    `Session::Menu` on touch. Portrait shows `#zz-landscape-hint` via CSS
    (`body.touch` + `orientation: portrait`).

41. **Audio unlock:** `web/index.html` installs a one-shot
    pointer/keydown listener that resumes any `AudioContext` it can find
    and exposes `window.__zzUnlockAudio`. Rust `platform::unlock_audio()`
    (called on first gesture from `touch.rs`) is the second path; a quiet
    `Sfx::Click` is also queued to prime Bevy's audio graph inside the
    gesture frame.

### Touch automation anchors (`?touch=1`)

Window-relative centres (logical px). Stick uses fractions of window size;
buttons use fixed insets from the bottom-right:

| Control | Centre (logical) |
|---------|------------------|
| Stick   | `(0.18 * W, 0.72 * H)` — drag ≥ 0.35 × 64 px for a direction |
| FIRE    | `(W - 56, H - 72)` — hold to shoot |
| JUMP    | `(W - 168, H - 150)` |
| NADE    | `(W - 168, H - 72)` |
| MELEE   | `(W - 250, H - 150)` — ≥ 64 px, clear of FIRE/JUMP/NADE |
| RELOAD  | `(W - 100, H - 220)` — near ammo counter |

On a 1280×720 pane: stick **(230, 518)**, FIRE **(1224, 648)**, JUMP
**(1112, 570)**, NADE **(1112, 648)**, MELEE **(1030, 570)**, RELOAD
**(1180, 500)**. Aim: press-drag on the right half (e.g. start at
`(900, 360)`, drag horizontally).

42. **Rome EUR client (M11b / R2b):** `env_lighting(EnvKind::RomeEur)` is warm
    Mediterranean late-afternoon — sky `srgb_u8(168,196,230)`, golden sun
    `srgb_u8(255,236,200)` @ 12k lux from the southwest
    (`sun_to ≈ normalize(-18, 20, 14)`; map +z is south), ambient warm grey
    `srgb_u8(168,158,142)` @ 1000. Wall family bases use a travertine-cream
    palette (`family_base_colors`) instead of the pastel-city set.
    `ground_tex_res` / `occlusion_res` derive from `arena_half` (≥2 texels/m,
    cell ≤2 m, hard cap 2048) so small towns stay at 1024/256 while Rome's
    500 m arena still bakes sharp. Lobby env picker includes **ROME EUR**.
    Page footer (`#zz-map-attrib`) credits OpenStreetMap (ODbL).

43. **Per-env dressing (M12 / Phase D):** all baked in `rebuild_map_system`
    (nothing per-frame except one cloud-root transform).
    - **Palettes:** `family_base_colors(env)` is total over all five envs —
      Urban pastel city, Mountain dark timber + grey stone, Desert sand/ochre
      adobe, Sea blue-grey warehouses + bleached planks, Rome travertine.
      `env_texture_tint` multiplies into wall/cover/roof texture bake;
      Mountain/Sea buildings use plank grain, Desert flat adobe mottling.
    - **Window slits:** `place_window_slits` (pure) on Building-family boxes
      taller than 2.5 m — deterministic hash of wall index + face + cell;
      hard-capped at 512. Merged via `build_window_mesh` into **one** emissive
      unlit mesh + one material (`LinearRgba` warm ~5.5/3.6/1.4).
    - **Lit ads:** `ad_material` stays unlit (readable in shadow) with a mild
      emissive lift (`LinearRgba::rgb(0.55, 0.48, 0.40)`) so panels read as
      backlit signage in every env.
    - **Clouds (M16):** ShotAnte voxel clusters — ~9 groups of 2–4 white
      boxes (w 3–7 m, y 18–30), one merged mesh, unlit + `fog_enabled: false`,
      parented as `CloudDriftRoot`. `drift_clouds_system` only translates that
      root (sin/cos, ~0.04 rad/s).
    - **WebGL2:** still no texture view-format tricks (item 35); wall/ground
      images are plain `Rgba8UnormSrgb`. LineList edge trim works on WebGL2
      (1 px lines, same as legacy `LineBasicMaterial`).

45. **ShotAnte visual parity (M16):** port of legacy `concreteTexture` /
    `loadArena` paint recipe in `map_render.rs`.
    - **Textures:** 128px base + 900×2×2 grain + 2px seam grid (`panels`,
      `grain`); ground tiles at 2 m with baked shadow multiply; perimeter /
      building / crate exact legacy colour combos; env tint wash on non-urban.
    - **Urban hues:** two distinct pastels from
      `[0xe8d8b8, 0xd9c4ad, 0xc9d6c2, 0xc4cede, 0xdcc6c6, 0xcfd8c0]` via
      `"{seed}-paint"` Mulberry32; BuildingA/B by `(x0+z0)>0`; roof flat
      `0x4b5364`. Other envs keep family base identity + seam-grid style.
    - **Edge trim:** Cover only; small (w,d ≤ 1.3) → map accent LineList;
      bulky → `0x6b542f`; scale 1.003; two draw calls. Buildings/roofs/perim
      excluded. Headers/sills (`y0≈0,y1≈1.3`) classify as Building.
    - **Lambert-flat:** `reflectance: 0.0` + high roughness on map materials.
    - **Barrels:** skipped — `GameMap` has no barrel list yet (only walls).

### Lobby env-picker automation anchors (1280×720)

Lobby card is centered (420 px wide). After host → lobby, the ENVIRONMENT
row sits near the lower-middle of the card. Approximate click centres:

| Env button | Centre (logical px @ 1280×720) |
|------------|--------------------------------|
| URBAN      | **(500, 455)** |
| MOUNTAIN   | **(575, 455)** |
| DESERT     | **(655, 455)** |
| SEA        | **(720, 455)** |
| ROME EUR   | **(800, 455)** — wraps to next line if tight: **(520, 485)** |

Prefer a 1 px `left_click_drag` (item 36) so egui registers the press.

43. **Proximity voice (M13 client):** open-mic once unmuted, **muted by
    default** (privacy). **M** toggles mute in-match. Mic permission is
    requested on the **first unmute only** — never at startup.
    - Wire: `BIN_VOICE=2` frames `[tag, slot, 16 kHz mono i16 LE PCM ≤3840 B]`
      (~120 ms = 1920 samples). Client sends `encode_voice(0, pcm)`; server
      rewrites slot authoritatively and fans out within
      `CHAT_PROXIMITY_RADIUS` (25 m).
    - Capture: wasm → `window.__zzVoice` in `web/index.html` (inline
      AudioWorklet + resample/frame; `platform::voice_*` polls). Native →
      `cpal` input (target-gated, not on wasm). Both feed the same frame shape.
    - Playback: per-speaker jitter buffer (2–4 frames) in `voice.rs`;
      distance fade starts at 10 m, silence at 25 m; equal-power L/R pan from
      listener yaw + remote interpolated positions. Chunked stereo
      `Decodable` one-shots on the zero-asset Bevy audio path.
    - HUD: tiny **MIC · MUTED / ON / LIVE / … / DENIED** chip at the top of
      the bottom-left vitals column (`left: 16`, `bottom: 28` desktop /
      lifted with health in touch mode). Optional roster speaking highlight
      is not wired (stretch).
    - Nothing blocks a frame; capture/play queues drop oldest on overflow.
    - Unit tests cover jitter OOO, fade/pan bounds, and fake-capture
      encode→decode→jitter round-trip (no real mic required in CI).

44. **Match-flow / rematch (M14):** first-match map build uses a durable
    `BuiltMapKey` (seed+env) rather than only `Res::is_changed`, so a
    `CurrentMap` inserted after the rebuild system in the same frame still
    builds on the next tick (and recovers if `MapRoot` is missing). Rebuild
    runs in `Update` **after** `GameSessionSet`. Rematch: client resets the
    snapshot decoder on every `GameStart` **inside `NetClient::drain` before
    decoding later binaries in the same poll** (resetting after decode dropped
    the room keyframe — M14b), clears `LatestSnapshot` / `LastStats` /
    `FxQueue`, and despawns remotes; server **stops snapshot broadcast after
    `MatchEnd`** so the old room cannot corrupt the next room's stream on the
    shared `OutMsg` channel. `MatchStats.time_alive_ms` is clamped to
    `duration_ms`. Snapshots apply HUD/self-sync without requiring
    `CurrentMap` (Commands insert is end-of-stage); `fps_controller` sends
    idle inputs as soon as `predicted.synced` even if walls are not loaded yet.
    Headless coverage: `crates/zz-client/tests/match_flow.rs` +
    `bot_horde::rematch_resets_peak_horde_and_time_alive_bounded` +
    idle/zero-input engagement suite (M14b).

45. **Ended-state perf (M14 / S7):** while `Session::Ended`, do **not** run
    remote interpolation, rig animation, growls, FX drain, or FPS controller
    — only camera + crumple cleanup + stats overlay. A stale horde + full
    interp was the 3–5 fps OVERRUN collapse.

46. **egui same-frame click (item 36 still applies):** bevy_egui multipass can
    drop a press+release that lands in one frame (trackpads + automation).
    Prefer 1 px `left_click_drag`. Bevy UI buttons (stats BACK) edge-detect
    without requiring `Changed<Interaction>` so same-frame presses still fire.
    Transient garbled egui glyphs ("n ob") are an upstream atlas quirk —
    they clear on the next full repaint; not client-owned.

47. **Space jump + prediction (M20):** browser Space is captured in
    `web/index.html` (capture-phase `keydown`/`keyup` → `window.__zzKeys.space`
    with `preventDefault`) and ORd into `PlayerInput.jump` via
    `platform::js_space_pressed()` alongside winit `KeyCode::Space` and
    `TouchIntent.jump`. On enter-Playing, `platform::refocus_canvas()` blurs
    egui text agents / closed overlays and focuses `#zz-canvas`. Once per
    match, if the JS bridge saw Space but Bevy `ButtonInput` never did, the
    console logs `winit missed Space — focus/default-action issue`. Local
    `step_body` runs only when `predicted.synced` **and** `CurrentMap` exists
    (still *sends* idle inputs pre-map — M14b); predicting against empty walls
    was the start-of-match rubber-band. Headless:
    `game_start_snapshot_syncs_and_queues_input` (fixed `Time` steps, no
    wall-clock), `prediction_waits_for_walls_then_matches_server`; server
    `bot_match::jump_input_raises_player_y`.
