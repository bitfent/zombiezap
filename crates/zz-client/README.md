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

### Smaller release wasm

Workspace root defines `[profile.wasm-release]` (`opt-level = "z"`, fat LTO,
strip). Prefer that for shipping:

```bash
cd web
# if your Trunk version supports cargo profiles:
trunk build --release --cargo-profile wasm-release
```

Debug `trunk build` is enough to prove the pipeline; wasm-release is for size.

The HTML canvas id is `zz-canvas`. Bevy’s primary `Window` is configured with
`canvas: Some("#zz-canvas")`, `fit_canvas_to_parent: true`, and
`prevent_default_event_handling: true`.

## Feature list (Bevy 0.19)

Curated in this crate’s `Cargo.toml` (workspace pins `bevy = "0.19"` with
`default-features = false`):

| Area | Features |
|------|----------|
| App core | `std`, `async_executor`, `multi_threaded`, `bevy_asset`, `bevy_log`, `bevy_state`, `reflect_auto_register` |
| Window / platform | `bevy_window`, `bevy_winit`, `x11`, `wayland`, `webgl2`, `default_font` |
| Render / PBR | `bevy_render`, `bevy_core_pipeline`, `bevy_pbr`, `bevy_light`, `bevy_camera`, `bevy_mesh`, `bevy_material`, `bevy_image`, `bevy_shader`, `bevy_color`, `tonemapping_luts`, `ktx2`, `zstd_rust`, `png` |
| UI / text | `bevy_text`, `bevy_ui`, `bevy_ui_render` |

Explicitly **not** enabled: `bevy_gltf`, `bevy_animation`, `bevy_audio` / `audio`,
`scene`, `webgpu` (browser target is WebGL2 only).

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

6. **`tonemapping_luts` is required** for non-pink PBR unless you change the
   camera’s `Tonemapping` method. Easy to miss when disabling defaults.

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

13. **LUT stack needs `ktx2` + a zstd backend.**  
    Enabling `tonemapping_luts` without `zstd_rust` (or `zstd_c`) fails at
    compile time inside `bevy_image`.

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
