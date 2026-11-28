#!/usr/bin/env python3
"""Dark mode sheet captures (Settings → Display → Dark mode): pw6 in portrait
and landscape, where labels stand beside the controls, and a 6" Basic without
eclipse waveforms, where Updates is hidden. One pw6 scene shows a hand-made
mix (Custom).

Run `cargo build` first. Output and disposable state stay under
screenshots/dark-mode.
"""
import sim

OUT = sim.out_dir('dark-mode', clean=True)

# Where the Display tab's Dark mode row is, by (device, orientation).
DARK_ROW = {
    ('pw6', 'portrait'): (636, 910),
    ('pw6', 'landscape'): (1270, 692),
    ('basic-167', 'portrait'): (300, 506),
}


def capture(name, device, orientation, settings=None):
    w, h, dpi = sim.DEVICES[device]
    s = sim.Screen(*((w, h) if orientation == 'portrait' else (h, w)), dpi)
    content_w = s.w - 2 * s.mm(5)
    display_tab = s.tap(s.mm(5) + content_w * 1.5 / 5, s.mm(13) + s.mm(5) + s.mm(4))
    data = sim.state(OUT / 'state' / name, None, {'orientation': orientation, **(settings or {})})
    steps = [*display_tab, *s.tap(*DARK_ROW[(device, orientation)])]
    sim.run(OUT / f'{name}.png', steps, data, device=device, screen='settings', quiet=False)


if __name__ == '__main__':
    capture('pw6-portrait', 'pw6', 'portrait')
    capture('pw6-landscape', 'pw6', 'landscape')
    capture('pw6-portrait-custom', 'pw6', 'portrait',
            {'dark_page_flash': 24, 'dark_pictures': 'full', 'dark_rendering': 'driver_night'})
    capture('basic-167-portrait', 'basic-167', 'portrait')
    print('captured', sorted(p.name for p in OUT.glob('*.png')))
