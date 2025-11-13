# Matterhorn AH

Matterhorn AH is a real-time fractal studio built with egui/eframe. It pairs an interactive viewport with timeline-driven animation, palette tooling, orbit traps, and an export pipeline that tiles frames before feeding them to FFmpeg.

## ASCII Fractal Snapshot

This Sierpinski-style triangle nods to the endlessly recursive geometry that Matterhorn AH explores:

```text
               *
              * *
             *   *
            * * * *
           *       *
          * *     * *
         *   *   *   *
        * * * * * * * *
       *               *
      * *             * *
     *   *           *   *
    * * * *         * * * *
   *       *       *       *
  * *     * *     * *     * *
 *   *   *   *   *   *   *   *
* * * * * * * * * * * * * * * *
```

## Feature Highlights
- CPU renderer included, with optional wgpu-powered GPU mode (`--features gpu`) for larger scenes.
- Multiple fractal types (Mandelbrot, Julia, Burning Ship, Multibrot) with adjustable power, escape radius, and Julia `c`.
- Palette lab complete with presets, flipping/cycling utilities, and import/export of `.ahpal` files.
- Orbit traps (point, circle, cross) for advanced coloring tricks.
- Timeline UI containing draggable keyframes, easing per key, an “Endless Zoom” preset, and an auto-place option that continuously locks the camera onto a repeating Seahorse Valley minibrot so infinite zooms keep looping seamlessly.
- Project persistence to JSON or TOML (`.mahproj`) plus palette sharing files.
- Video export to H.264, ProRes, VP9, or AV1 via FFmpeg, with headless CLI support.

## Getting Started

### Prerequisites
- Latest stable [Rust](https://rustup.rs/) toolchain (the project targets Rust 2021).
- `ffmpeg` available on your `PATH` for video exports.
- (Optional) A Vulkan/Metal/DX12-capable GPU if you plan to build with the `gpu` feature.

### Run the App
```sh
cargo run
```
Pass `--release` for higher frame rates, or enable the wgpu backend:
```sh
cargo run --release --features gpu
```
When started without arguments the UI boots with default parameters. Use `-p some_project.mahproj` (or `--project`) to load an existing scene at launch.

## Using the UI
- **Top bar** – Playback controls, save/load project buttons, export trigger, and a backend selector that lets you toggle CPU/GPU rendering.
- **Side panel** – Organized sections for fractal parameters, camera controls, palette editing, orbit trap options, and export settings (resolution, duration, fps, codec, tile size, output path).
- **Viewport** – Live render of the selected fractal with whatever animation state is currently in effect.
- **Timeline panel** – Manage animation keyframes for zoom, palette phase, and camera center coordinates. Double-click to add a key, drag to re-time, right-click to delete, or cycle easing by double-clicking an existing key.

### Endless Zoom & Repeating Spot
Use the “Preset: Endless Zoom” button to convert the zoom track into an open-ended exponential zoom. Enabling **Auto-place repeating spot** snaps the camera onto a hand-tuned Seahorse Valley minibrot, keeps the camera centered there, and reuses that spot for perfect-looking infinite zooms. “Re-center to repeating spot” performs the snap again if you have drifted away, while “Re-base” simply copies the current scale into the zoom preset without changing the camera position.

## Projects, Palettes, and Files
- **Projects** – Save to JSON or `.mahproj` (TOML). Each file packs fractal settings, timelines, export presets, and render backend choice.
- **Palettes** – Export/import `.ahpal` JSON files from the palette panel.
- **Headless exports** – Use `cargo run --release -- export --project scenes/demo.mahproj --out render.mp4` to render without opening the UI. The CLI command tiles the render into PNG frames, then invokes FFmpeg with the codec-specific arguments shown in the UI.

## Video Export Workflow
1. Configure resolution, fps, duration, codec, CRF, and tile size inside the Export panel.
2. Click **Export Video** (UI) or run the CLI command above. Frames are rendered into a temp dir before FFmpeg muxes them into the selected container/codec.
3. Ensure FFmpeg is installed; otherwise the export command returns `ExportError::Ffmpeg`.

## License

Dual-licensed under MIT or Apache-2.0. Use whichever license better suits your project.
