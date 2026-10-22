# KRB-Renderer viewer: English (the reference language; every message must be here)

## Menu: File
menu-file = File
menu-save-image = Save image
menu-save-image-tip = Save a numbered PNG and EXR in the board folder (or where --out points)
menu-save-png = Save as PNG...
menu-save-png-tip = Choose the place and name (exposure, glow and tone mapping applied)
menu-save-exr = Save as EXR...
menu-save-exr-tip = Choose the place and name (linear radiance; no exposure or glow)
menu-settings-location = Board settings file location
loc-next-to-board = Next to the board (<name>.krbr)
loc-next-to-board-tip = In the same folder as the KiCad board, so it travels with the board
loc-config-dir = Global settings folder (%APPDATA%\krbr\boards)
loc-config-dir-tip = Adds no files to the KiCad folders
loc-hint = Applies to every board. Files of other boards are moved the next time they are opened
menu-reveal-settings = Show the board settings file
menu-reveal-settings-tip = Show this board's <name>.krbr in Explorer (if there is none yet, open the folder it goes in)
menu-quit = Quit

## Menu: View
menu-view = View
menu-settings-panel = Settings panel
menu-pause = Pause rendering
menu-render-size = Rendering size
menu-render-ratio = Rendering ratio
menu-reset-view = Reset view
menu-auto-exposure = Auto exposure
menu-reset-all = Reset all settings to defaults
menu-reset-all-tip = Set the panel settings (environment map, exposure, integrator, lens, color computation, surface, ...) back to the values of the settings files and the command line, and forget what was remembered for this board
menu-language = Language
language-auto = Automatic
language-auto-tip = The language chosen in KiCad, otherwise the language of the OS

## Menu: Help
menu-help = Help
help-rotate = Left drag: rotate
help-select = Click: select a part (change its materials in the settings panel)
help-pan = Right / middle drag: pan
help-zoom = Wheel: zoom
help-focus = Ctrl + click: focus on that point
# Keys (the key names come from the program; these are the descriptions)
key-panel = settings panel
key-reset-view = reset view
key-save = save the image
key-pause = pause
key-denoise = denoise now (when denoising runs on the CPU)
key-exposure = exposure
key-env-rotation = rotate the environment
key-display-mode = image / albedo / normal
key-quit = quit

## Toolbar
toolbar-done = ✔ Done
toolbar-done-tip = The maximum spp is reached, so rendering has finished and stopped. To keep going, raise the maximum spp in the settings panel
toolbar-resume = ▶ Resume
toolbar-pause = ■ Pause
toolbar-pause-tip =
    Pause rendering (P). No samples are added, so the GPU is almost idle and the controls feel lighter.
    When the camera moves, one sample is rendered and then it stops again

## Status bar
status-paused = Paused
status-done = Done
status-rendering = Rendering
status-elapsed = Elapsed { $time }
status-elapsed-tip = Rendering time since the current accumulation started (pauses and the time after it finished are not counted; restarts from 0 when the view changes)
status-average = Average { $wall } ms/spp (GPU { $gpu } ms)
status-average-tip = Time per sample: elapsed time ÷ samples (in parentheses: only the time the GPU spent rendering samples)
status-remaining = About { $time } left
status-integrator = { $integrator } · { $color }
status-spectral = Spectral
status-gpu = { $name } ({ $method })
status-render-size = Render { $width }×{ $height } (display { $percent }%)
status-size-fixed-tip = A fixed size (View → Rendering size in the menu)
status-size-fit-tip = Fit to the window (3D view area × rendering ratio)
status-reloading = Reloading the board...
status-silk = Exporting the stroke-font text (kicad-cli)...
status-meshing = Meshing 3D models: { $count } (shown as boxes until then)
duration-seconds = { $seconds } s

## Settings panel
panel-settings = Settings
panel-close-tip = Close (show it again from View in the menu, or with F1 / Tab)
panel-help-mouse = Left drag: rotate  Right drag: pan  Wheel: zoom  Click: select a part
panel-help-toggle = F1 / Tab: show or hide this panel

panel-environment = Environment map
env-builtin = { $id } (built-in)
env-builtin-studio = Studio (built-in)
env-builtin-studio-tip = A grey photo booth with a large softbox
env-gpu-failed = Could not send the environment map to the GPU, so the built-in studio is used: { $error }
env-open = Open...
env-open-tip = Choose a .hdr / .exr (you can also drop one onto the window)
env-remember-hint = The choice is also used the next time this board is shown
env-loading = Loading: { $path }
env-intensity = Intensity
env-rotation = Rotation
env-not-an-environment = Not an environment map (.hdr / .exr): { $path }
dlg-open-env-title = Open environment map
dlg-env-filter = Environment maps (*.hdr, *.exr)

panel-exposure = Exposure
exposure-auto = Auto
glow = Glow
glow-tip = How strongly very bright lights, such as lit LEDs, bleed into their surroundings (0: none). Also applied to saved images
glow-threshold = Threshold
glow-threshold-tip = Only lights brighter than this bleed (the RGB maximum after exposure; 1 is the white of the screen). Raise it when white boards or parts start to bleed
glow-spread = Spread
glow-spread-tip = How far the glow spreads. Small values keep it close to the light; 1 spreads it faintly over nearly the whole screen
denoise = Denoise (OIDN)
denoise-now = Apply now (D)
denoise-unavailable = Not available
denoise-strength = Strength
denoise-strength-tip = How much of the denoised image to blend in (1: the OIDN result as is; lower values keep a little detail and grain)
denoise-cpu-hint = Running on the CPU, so it is applied only when rendering finishes and on the D key

panel-integrator = Integrator
integrator-pt-hint = Unidirectional (fast)
integrator-bdpt-hint = Bidirectional (connects to paths from the lights)
integrator-vcm-hint = Bidirectional + vertex merging (also converges caustics through transparent objects)
integrator-failed = { $integrator } is not available, so PT is used: { $error }

panel-lens = Lens
lens-pinhole = Pinhole
lens-thin = Thin lens
lens-realistic = Real lens
lens-f-number = F-number
lens-focus = Focus (mm)
lens-focus-hint = Ctrl + click: focus on that point
lens-focal-length = Focal length (mm)
lens-blades = Aperture blades
lens-blade-rotation = Blade rotation
lens-blade-roundness = Blade roundness
lens-natural-vignetting = Natural vignetting (cos⁴)
lens-optical-vignetting = Optical vignetting
lens-axial-ca = Axial chromatic aberration (Abbe number)
lens-lateral-ca = Lateral chromatic aberration
lens-ca-hint = Chromatic aberration needs the spectral mode
focus-set = Focused at { $mm } mm

panel-color = Color computation
color-spectral = Spectral (4 wavelengths)

panel-surface = Surface
bump-intensity = Bump strength
mask-as-configured = As configured
mask-hint = Solder mask color (preset)
mask-note-green = KiCad / the common green
mask-note-jlcpcb-green = JLCPCB Green: a slightly lighter green, glossy
mask-note-jlcpcb-purple = JLCPCB Purple: a bluish purple
mask-note-jlcpcb-white = JLCPCB White: nearly opaque
mask-note-jlcpcb-black = JLCPCB Black: glossy black

panel-display = Display
size-fit-window = Fit to window
size-preset = { $size } ({ $name })
size-full-hd = Full HD
size-square = square
size-hint = Rendering size (a fixed size resizes the window to fit)
ratio-label = ×{ $ratio } ({ $what })
ratio-coarse = coarse, fast
ratio-fine = rendered finer and scaled down
ratio-native = 1:1
ratio-hint = Rendering ratio (rendering resolution ÷ display size)
fit-too-large = { $size } does not fit on the screen, so the rendering ratio was set to ×{ $ratio } (display { $percent }%)
mode-image = Image
mode-albedo = Albedo
mode-normal = Normal
max-spp = Max spp

panel-camera = Camera
camera-fov = Field of view
camera-angles = Elevation { $elevation }°  Azimuth { $azimuth }°
camera-reset = Reset view (R)
camera-save = Save (S)

## Selected part
sel-title = Selected part
sel-none-hint = Click a part to change its materials
sel-deselect = Deselect
sel-no-outline = The outline is not shown with the real lens
sel-meshing = Meshing the 3D model (shown as a box until done)
sel-missing = There is no 3D model, so a placeholder box is shown (its materials cannot be changed)
sel-missing-model =
    { $spec }
    → { $resolved } ({ $reason })
sel-missing-hint = Add places to search with [paths] model_search_dirs in the board settings file <name>.krbr
scope-part = This part
scope-part-tip = Only this part on this board ([ref.<reference>] in the board settings file <name>.krbr)
scope-model = This model
scope-model-tip = Every part on this board that uses the same 3D model ([[model]] in <name>.krbr)
scope-all-boards = All boards
scope-all-boards-tip = Every board that uses the same 3D model (%APPDATA%\krbr\model_map.toml)
sel-shadowed-ref = This part has an override by reference, which takes precedence (clear it with "This part" to see the change)
sel-shadowed-project = This project has a setting for the model, which takes precedence (clear it with "This model" to see the change)
part-whole = (whole)
part-source-tip =
    { $keys }
    Decided by: { $source }
part-pattern = Pattern
part-pattern-tip = The surface pattern (fine bumps and uneven roughness). Without it the surface is smooth (adds +smooth to the material name)
part-emit = Emit
part-emit-tip = Make this part glow (a lit LED, for example). Change the color and brightness on the row below (adds +emit=#rrggbb,<cd/m²> to the material name)
emit-color-tip = Emission color
emit-luminance-tip = Emission brightness (luminance). Drag or type to change it. Where it exceeds the glow threshold, as a multiple of the screen white (1) after exposure, it bleeds (5000 at an exposure of 0.7 EV is about 8 times). Higher values make the core whiter
parts-all = All parts
parts-choose-all = Choose for all
parts-pattern-all-tip = Toggle the surface pattern of all parts at once
sel-source = Decided by: { $source }
sel-source-varies = Decided differently per part (hover over a part name to see it)
sel-clear-override = Clear the override (back to the rules)
sel-clear-override-tip = Remove this part's (model's) setting from the selected destination
material-set-all = Set all parts of { $reference } to { $material } ({ $file })
material-set-part = Set { $part } of { $reference } to { $material } ({ $file })
material-cleared-all = Cleared the override of all parts of { $reference } ({ $file })
material-cleared-part = Cleared the override of { $part } of { $reference } ({ $file })
material-nothing-to-clear = There is no override to clear (it already follows the rules)

## Messages
reset-all-done = All settings were reset to the defaults
reload-thread-failed = Reloading failed, so watching for saves was stopped
reload-error = Could not reload (waiting for the next save): { $error }
reload-done = Reloaded: { $changed } changed, { $added } added, { $removed } removed{ $full }, { $tiles } tiles (parse { $parse } ms, build { $build } ms, GPU { $gpu } ms{ $since })
reload-full = {" "}(full)
reload-since = , { $seconds } s after the save
silk-applied = Applied the stroke-font text (kicad-cli)
models-applied = Applied 3D models ({ $pending } remaining)
models-all-applied = Applied all 3D models
settings-location-failed = Could not change the location: { $error }
settings-location-changed = Changed the location of the board settings file: { $file }
settings-location-not-yet = Changed the location of the board settings file (this board has none yet; it will be created at { $file } when written)
dlg-save-png-title = Save as PNG
dlg-png-filter = PNG image (*.png)
dlg-save-exr-title = Save as EXR
dlg-exr-filter = OpenEXR, linear (*.exr)
saved = Saved: { $path } (and .exr, { $spp } spp)
saved-denoised-too = . The denoised image was saved too (_denoised)
saved-as = Saved: { $path } ({ $spp } spp{ $denoised })
saved-as-denoised = , denoised
save-failed = Could not save: { $error }
readback-failed = Could not read the image back from the GPU: { $error }
materials-failed = Could not send the materials to the GPU: { $error }
