---
name: testing-darask-paint-windows-gui
description: Verify the Windows desktop editor, exports and local plugin transport, using software OpenGL when the VM driver cannot render.
---

# Windows GUI testing

## Build and rendering

- No credentials are needed for editing and exports. Real AI inference requires separately installed models; do not assume a backend is running.
- Follow `CLAUDE.md` for build and test commands. Run Cargo after Visual Studio's `VC/Auxiliary/Build/vcvars64.bat` so Git Bash's `link.exe` does not shadow the MSVC linker.
- If launch fails with `egui_glow requires opengl 2.0+`, try the interactive desktop session first.
- If necessary, use an official `pal1000/mesa-dist-win` x64 release and place `opengl32.dll` and `libgallium_wgl.dll` beside the release executable. Mesa 26.2.0 release-msvc worked in testing. Keep this local to the test executable; never replace Windows system DLLs.
- Extract Mesa archives with 7-Zip (`choco install 7zip -y`). Use PowerShell when passing Windows output paths to `7z`.
- Maximize the application before recording. Collapse the sidebar's 色 header to expose the layer controls.

## Golden paths

- Ctrl+N creates a document with custom dimensions and white or transparent background.
- Brush size, hardness and opacity expose editable numeric values. Number keys 1–9 select 10–90% opacity; 0 selects 100%.
- M selects a rectangle, E selects the eraser, and Ctrl+D deselects.
- Verify overlapping strokes, undo/redo, layer opacity, visibility, blend modes and reordering.
- Ctrl+Shift+S opens project/image export. Choose PNG/JPEG/BMP in the file-type dropdown; changing only the extension is insufficient. JPEG shows a quality confirmation.
- Reopen exports. PNG retains alpha; JPEG/BMP flatten onto white. System.Drawing can compare BMP RGB against PNG RGBA composited over white without Python packages.

## Authorized loopback mock

- The AI・修復 menu contains IOpaint and Diffusion actions. Inpaint requires an active selection.
- Defaults: IOpaint port 8423, Diffusion port 8424, always on 127.0.0.1.
- IOpaint `GET /api/v1/health` requires `plugin="darask-iopaint"`, `api=1`, `backend="ready"`, `model="lama"` and an `engine` string.
- `POST /api/v1/inpaint` contains base64 `image` (active-layer RGBA8, not the visible composite) and `mask` (grayscale8) PNGs.
- Request dimensions equal the selection bbox plus 128px margins, clipped to the document. Return a PNG with those dimensions.
- Verify a solid-color response changes only selected pixels and undo/redo restores both states. Label mock testing separately from model inference.
- Stop the mock listener when done.

## Idle and evidence

- Deselect to stop marching ants, move the pointer off the app, and let toasts expire. Sample process `TotalProcessorTime` over at least 10 seconds.
- Report the Mesa fallback, any corrected test fixtures, and untested real inference paths. Preserve full screenshots and recordings.

## Real ZIP plugin workflow

- Use a clean copy of the release executable, with distribution ZIPs in an adjacent `plugins` folder. The first AI action extracts all direct-child ZIPs into stem-named folders and writes `.darask-zip-stamp`; only the requested engine's launcher should start.
- Ctrl+K opens preferences. Test `プラグインフォルダ` with a path containing spaces, save, restart, and verify both persistence and extraction in that location while the engine is stopped. A running healthy engine bypasses folder lookup.
- Start the GUI without redirecting standard handles when recording launcher consoles. Redirected handles can be inherited by the launcher, making its new console blank even though setup is running.
- On clean Windows VMs, CPU torch may fail loading `c10.dll` with WinError 1114 if the Visual C++ runtime is outdated. Check `vcruntime140_1.dll`; the current official x64 redistributable (`https://aka.ms/vs/17/release/vc_redist.x64.exe`, `/install /quiet /norestart`) resolved this in testing.
- IOpaint downloads dependencies and LaMa on first run. Diffusion prompts to download a checkpoint; answer in the visible console. Its approximately 2GB checkpoint may download quickly, but CPU inference still needs separate verification.
- Check real health responses, not merely open ports: IOpaint 8423 must report backend `ready` and model `lama`; Diffusion 8424 must report backend `ready` with a checkpoint. An adapter process may remain alive after ComfyUI crashes. Inspect `%LOCALAPPDATA%\DaraskAIDiffusion\comfyui.log` for the actual failure.
- CPU ComfyUI generally requires `--cpu`; CPU torch installation alone does not prove CPU engine startup. Verify the launched command and a real image result.
- Distinguish startup polling from inference-response timeouts. CPU generation can exceed two minutes even at 256x256/20 steps; compare ComfyUI's `Prompt executed in ...` log with whether the editor actually applies the result. Backend completion alone is not an end-to-end pass. The generation dialog has no step-count control, so use smaller canvas dimensions when needed.
- For readable generated-image evidence, use the View menu's fit-to-window action (the menu also shows the Home shortcut), rather than painting/panning with an unverified gesture. Verify Undo restores the previous canvas and Redo restores inference output.
- Initial setup may exceed the editor's 120-second polling window. Confirm the Japanese retry message, allow the launcher to continue, and retry the AI action after health is ready. Report backend-error states separately from genuine setup-in-progress.
- If repository policy blocks the pinned IOpaint Git source, ask the lead for approved access or a verified local mirror of the exact tag. Any mirror URL rewrite should be process-scoped and disclosed as a testing-environment workaround; do not edit global Git configuration.

## Devin Secrets Needed

- None for the local GUI and public model downloads. Repository access for the pinned IOpaint source must be available through the session's approved Git access or a lead-provided verified mirror.