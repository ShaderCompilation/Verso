#!/usr/bin/env python3
"""Simulator screenshots of the Wi-Fi transfer screen in every state, and
the two ways in: Settings → Device and the library's top bar.

Run cargo build first. Outputs live in gitignored screenshots/transfer/.
VERSO_SIM_TRANSFER sets each state up (see transfer/device.rs): `kindle`
shows a Kindle's name and address, `code=` fixes the code, `activity`
fills the list and `phase=` shows a state without starting anything. The
ready states really serve on 127.0.0.1 (VERSO_TRANSFER_PORT, default 8080).
"""
import sim

OUT = sim.out_dir('transfer')

TRANSFER = {
    'ready': 'kindle,code=4827,activity',
    'ready-empty': 'kindle,code=4827',
    'ready-simulator': 'code=4827',
    'starting': 'phase=starting',
    'waiting': 'phase=waiting',
    'no-network': 'phase=no-network',
    'stopped-idle': 'phase=idle',
    'stopped-slept': 'phase=slept',
    'failed-wifi': 'phase=no-wifi',
    'failed-port': 'phase=port',
}

# The ways in, portrait: Settings → Device, and Home's top bar.
ENTRIES = {
    'entry-settings': ['--screen', 'settings', '--tap', '867,272'],
    'entry-home': ['--screen', 'books'],
}


def main():
    books = sim.books(OUT / 'books', 'search.epub')
    for name, options in TRANSFER.items():
        capture(f'portrait-{name}', 'portrait', options, ['--screen', 'transfer'], books)
    for name in ['ready', 'no-network', 'failed-port']:
        capture(f'landscape-{name}', 'landscape', TRANSFER[name], ['--screen', 'transfer'], books)
    # Done after Verso's data was edited (developer mode): Restart, or Exit
    # under a launcher older than protocol 2.
    done = ['--screen', 'transfer', '--tap', '636,1566']
    restart = 'kindle,code=4827,edited'
    capture('portrait-restart', 'portrait', restart, done, books, {'VERSO_LAUNCHER_PROTOCOL': '2'})
    capture('portrait-restart-exit', 'portrait', restart, done, books)
    capture('landscape-restart', 'landscape', restart, ['--screen', 'transfer', '--tap', '1400,1150'], books,
            {'VERSO_LAUNCHER_PROTOCOL': '2'})
    for name, steps in ENTRIES.items():
        capture(name, 'portrait', '', steps, books)
    print(OUT)


def capture(name, orientation, options, steps, books, extra_env=None):
    state = sim.state(OUT / 'state' / name, books, {'orientation': orientation})
    sim.run(OUT / (name + '.png'), steps, state, books, env={'VERSO_SIM_TRANSFER': options, **(extra_env or {})})


if __name__ == '__main__':
    main()
