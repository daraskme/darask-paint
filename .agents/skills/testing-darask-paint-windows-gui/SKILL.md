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