"""Regression checks for measurement boundaries, using synthetic traces only."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('analyzer', Path(__file__).with_name('analyze-redraw.py'))
analyzer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(analyzer)
STARTUP_SPEC = importlib.util.spec_from_file_location('startup_analyzer', Path(__file__).with_name('analyze-startup.py'))
startup_analyzer = importlib.util.module_from_spec(STARTUP_SPEC)
STARTUP_SPEC.loader.exec_module(startup_analyzer)


def stat(ticks):
    fields = ['0'] * 40
    fields[11] = str(ticks)
    return '42 (verso) ' + ' '.join(fields)


def fixture():
    trace = ['0 MARK 1 turns_start']
    for i in range(24):
        release = 1 + i
        send = release + (.11 if i == 1 else .01)
        trace.extend([f'{release} MARK 1 turn_{i + 1}',
                      f'{send} IOCTL 42 4048462e {send + .001} 0 {i + 1} 2 {int(i == 0)} 0 0 1272 1696'])
        if i == 0:
            trace.append('1.02 IOCTL 42 c004462f 2.10 0 1 0 0 0 0 0 0')
    trace.append('25 MARK 1 turns_end')
    return dict(variant='synthetic', trace='\n'.join(trace), page_hash='same', restored_hash='same',
                frames='frame: drawn in 1ms, diffed in 1µs, 1 update(s)\n' * 24,
                start_stat=stat(100), end_stat=stat(150),
                start_tasks=stat(100), end_tasks=stat(150),
                idle=[stat(150), '100 50', stat(150), '112 50'])


class Measurements(unittest.TestCase):
    def summarize(self, data):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'run.json'
            path.write_text(json.dumps(data))
            return analyzer.summarize(path)

    def test_prior_flash_overlap_and_idle(self):
        run, events = self.summarize(fixture())
        self.assertAlmostEqual(events[1]['prior_flash_wait_ms'], 100)
        self.assertAlmostEqual(events[0]['prior_flash_wait_ms'], 0)
        self.assertEqual(run['idle_cpu_ticks'], 0)
        self.assertEqual(run['idle_seconds'], 12)
        self.assertEqual(run['cpu_seconds'], .5)

    def test_reject_update_associated_with_later_input(self):
        data = fixture()
        data['trace'] = data['trace'].replace('1.01 IOCTL', '2.01 IOCTL')
        with self.assertRaises(AssertionError):
            self.summarize(data)

    def test_reject_update_overtaking_flash(self):
        data = fixture()
        data['trace'] = data['trace'].replace('2.10 0 1', '2.20 0 1')
        with self.assertRaisesRegex(AssertionError, 'overtook'):
            self.summarize(data)

    def test_reject_missing_turn(self):
        data = fixture()
        data['trace'] = data['trace'].replace('24 MARK 1 turn_24', '24 MARK 1 discarded')
        with self.assertRaises(AssertionError):
            self.summarize(data)

    def test_rejected_capture_never_enters_statistics(self):
        data = fixture()
        data['capture_error'] = 'saved page did not advance'
        with self.assertRaisesRegex(AssertionError, 'Rejected capture'):
            self.summarize(data)

    def test_queue_requires_page_advancement(self):
        data = fixture()
        data.update(queued_input=True, initial_position={'page': 10}, saved_after_turns={'page': 33})
        with self.assertRaises(AssertionError):
            self.summarize(data)

    def test_queue_can_complete_during_the_next_contact(self):
        data = fixture()
        data.update(queued_input=True, initial_position={'page': 10}, saved_after_turns={'page': 34})
        data['frames'] += ' Tap(Point { x: 1, y: 1 })\n' * 24
        data['trace'] = data['trace'].replace('\n2 MARK', '\n2.1 MARK').replace('\n3 MARK', '\n2.9 MARK')
        data['trace'] = data['trace'].replace('2.11 IOCTL', '2.86 IOCTL').replace('2.111 0', '2.861 0')
        _, events = self.summarize(data)
        self.assertAlmostEqual(events[1]['request_ms'], 760)

    def startup_fixture(self):
        return dict(clean_exit=True, variant='synthetic', fixture='real', mode='startup',
                    timings=[], samples={}, trace='\n'.join([
                        '0 MARK 9 process_start',  # Inherited preload in another process.
                        '.2 MARK 42 process_start',
                        '.3 IOCTL 42 4048462e .301 0 7 2 1 0 0 1272 1696',
                        '.31 IOCTL 42 c004462f .61 0 7 0 0 0 0 0 0']))

    def test_startup_selects_reader_constructor_and_matching_completion(self):
        row = startup_analyzer.summarize(self.startup_fixture(), 'synthetic')
        self.assertEqual(row['startup_request_ms'], 100)
        self.assertEqual(row['startup_completion_ms'], 410)

    def test_startup_rejects_failed_display_update(self):
        data = self.startup_fixture()
        data['trace'] = data['trace'].replace('.301 0 7', '.301 -1 7')
        with self.assertRaises(AssertionError):
            startup_analyzer.summarize(data, 'synthetic')


if __name__ == '__main__':
    unittest.main()
