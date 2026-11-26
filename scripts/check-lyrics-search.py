#!/usr/bin/env python3
"""Check lookup latency and caches, locally or explicitly over the internet."""
import argparse
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time


FIXTURE = r'''#!/usr/bin/env python3
import json, os, sys, time
params = dict(arg.split('=', 1) for arg in sys.argv[1:] if '=' in arg and not arg.startswith('http'))
name = params.get('track_name', params.get('title', ''))
stage = 'lp' if 'lyricsplus' in sys.argv[-1] else ('search' if sys.argv[-1].endswith('/search') else 'get')
def log(event):
    with open(os.environ['OWN_PLAYER_CURL_LOG'], 'a') as output:
        output.write(json.dumps(dict(stage=stage, event=event, at=time.monotonic())) + '\n')
log('start')
delay = 0.02
if name in ('search_words', 'missing_search') and stage == 'lp': delay = 0.5
if name == 'search_error' and stage == 'search':
    print('{}'); print('500', end=''); sys.exit()
if name == 'slow_search' and stage == 'search': delay = 0.5
time.sleep(delay)
words = ((name == 'get_words' and stage == 'get') or
         (name in ('lp_words', 'slow_search') and stage == 'lp') or
         (name == 'search_words' and stage == 'search') or
         (name in ('missing_search', 'search_error') and stage == 'get'))
missing = name == 'not_found' or (name == 'missing_search' and stage == 'search') or stage == 'lp' and not words
if words and stage == 'lp':
    result = dict(type='Word', processingTime=dict(selectedSongMetadata=dict(title=name, artist='Fixture Artist', duration=20)), lyrics=[dict(time=1000, duration=3000, text='First second', syllabus=[dict(time=1000, duration=1000, text='First '), dict(time=3000, duration=1000, text='second')])])
else:
    result = dict(trackName=name, artistName='Fixture Artist', duration=20, syncedLyrics='[00:01]<00:01>First <00:03>second' if words else '[00:01]First second')
    if name == 'partial_words':
        result['syncedLyrics'] = ('[00:01]<00:01>First <00:03>second' if stage == 'get' else '[00:01]First second') + '\n[00:05]Third fourth'
print(json.dumps({} if missing else [result] if stage == 'search' else result))
print('404' if missing else '200', end='')
'''


def invoke(binary, case, env):
    started = time.perf_counter()
    result = subprocess.run(
        [str(binary), 'lyrics-check', '--online', '--title', case,
         '--artist', 'Fixture Artist', '--duration', '20'],
        env=env, text=True, capture_output=True, timeout=15,
    )
    elapsed = (time.perf_counter() - started) * 1000
    if case == 'not_found':
        if result.returncode != 1:
            raise AssertionError('not_found: expected an unsuccessful lookup')
        return elapsed, None
    if result.returncode:
        raise AssertionError(f'{case}: checker process failed')
    data = json.loads(result.stdout)
    expected = 2 if case in ('line_only', 'partial_words') else 3
    if data['quality'] != expected:
        raise AssertionError(f'{case}: quality {data["quality"]}, expected {expected}')
    if case == 'partial_words' and data['word_lines'] != 1:
        raise AssertionError('partial word cues were lost during lookup or cache restore')
    first = data.get('lookup', {}).get('first_result_ms')
    if first is None:
        match = re.search(r'emit quality=\d+ at (\d+) ms', result.stderr)
        first = float(match[1]) if match else None
    return elapsed, first


def check_live(args):
    """Alternate binaries with isolated cold caches; never print lyrics or logs."""
    binaries = {'current': args.binary.resolve()}
    if args.baseline:
        binaries['before'] = args.baseline.resolve()
    reports = []
    for title, artist, duration in [('Hello', 'Adele', 295),
                                     ('Never Gonna Give You Up', 'Rick Astley', 213),
                                     ('Blinding Lights', 'The Weeknd', 200)]:
        samples = {label: [] for label in binaries}
        for trial in range(args.runs):
            order = list(binaries)
            if trial % 2:
                order.reverse()
            for label in order:
                with tempfile.TemporaryDirectory(prefix='veyra-live-') as directory:
                    env = os.environ.copy()
                    env.pop('OWN_PLAYER_CURL_LOG', None)
                    env.update(XDG_CACHE_HOME=directory, XDG_DATA_HOME=directory,
                               VEYRA_HTTP_BACKEND='gio', VEYRA_LYRICS_TIMING='1')
                    command = [str(binaries[label]), 'lyrics-check', '--online', '--title', title,
                               '--artist', artist, '--duration', str(duration), '--summary']
                    started = time.perf_counter()
                    cold = subprocess.run(command, env=env, capture_output=True, text=True, timeout=40)
                    cold_ms = (time.perf_counter() - started) * 1000
                    if cold.returncode:
                        raise AssertionError(f'{label}: live lookup failed for {title}; stopping to avoid repeated requests')
                    data = json.loads(cold.stdout)
                    if data['quality'] < 2:
                        raise AssertionError(f'{label}: no synchronized result for {title}')
                    started = time.perf_counter()
                    warm = subprocess.run(command, env=env, capture_output=True, text=True, timeout=15)
                    warm_ms = (time.perf_counter() - started) * 1000
                    if warm.returncode or 'http host=' in warm.stderr:
                        raise AssertionError(f'{label}: live warm cache failed for {title}')
                    if json.loads(warm.stdout)['quality'] != data['quality']:
                        raise AssertionError(f'{label}: live warm cache changed quality for {title}')
                    samples[label].append(dict(cold_ms=cold_ms, warm_ms=warm_ms,
                                               first_ms=data['lookup']['first_result_ms'],
                                               quality=data['quality'], word_lines=data['word_lines'],
                                               invalid_lines=data['invalid_lines']))
        results = {}
        for label, values in samples.items():
            results[label] = {key: round(statistics.median(row[key] for row in values), 2)
                              for key in ['cold_ms', 'warm_ms', 'first_ms']}
            results[label].update(cold_range_ms=[round(min(row['cold_ms'] for row in values), 2),
                                                  round(max(row['cold_ms'] for row in values), 2)],
                                  quality=min(row['quality'] for row in values),
                                  word_lines=sorted(set(row['word_lines'] for row in values)),
                                  invalid_lines=sorted(set(row['invalid_lines'] for row in values)))
        if 'before' in results:
            if results['current']['quality'] < results['before']['quality']:
                raise AssertionError(f'live quality regressed for {title}')
            if min(results['current']['word_lines']) < min(results['before']['word_lines']):
                raise AssertionError(f'live word coverage regressed for {title}')
            results['speedup'] = round(results['before']['cold_ms'] / results['current']['cold_ms'], 2)
        reports.append(dict(track=title, results=results))
    print(json.dumps(dict(reference='live GIO/TLS; separate cold caches; alternating binaries',
                          runs=args.runs, results=reports), indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path(__file__).resolve().parents[1] / 'target/release/veyra')
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--enforce', action='store_true', help='also require early provider and fallback starts')
    parser.add_argument('--live', action='store_true', help='query real services for three public test tracks')
    parser.add_argument('--baseline', type=Path, help='with --live, alternate with this older binary')
    args = parser.parse_args()
    if args.runs < 1 or not args.binary.is_file():
        parser.error('need --runs >= 1 and a built --binary (cargo build --release --locked --bin veyra)')
    if args.baseline and (not args.live or not args.baseline.is_file()):
        parser.error('--baseline requires --live and an existing binary')
    if args.live:
        check_live(args)
        return
    binary = args.binary.resolve()
    reports = []
    for case in ['get_words', 'lp_words', 'search_words', 'missing_search', 'search_error', 'slow_search', 'line_only', 'partial_words', 'not_found']:
        cold, warm, first, requests = [], [], [], []
        for _ in range(args.runs):
            with tempfile.TemporaryDirectory(prefix='veyra-lookup-') as directory:
                root = Path(directory)
                fixture = root / 'bin/curl'
                fixture.parent.mkdir()
                fixture.write_text(FIXTURE)
                fixture.chmod(0o755)
                log = root / 'requests'
                env = dict(os.environ, PATH=f'{fixture.parent}:{os.environ.get("PATH", "")}',
                           XDG_CACHE_HOME=str(root / 'cache'), XDG_DATA_HOME=str(root / 'data'),
                           OWN_PLAYER_CURL_LOG=str(log), VEYRA_LYRICS_TIMING='1')
                elapsed, improvement = invoke(binary, case, env)
                cold.append(elapsed)
                if improvement is not None:
                    first.append(improvement)
                events = [json.loads(line) for line in log.read_text().splitlines()]
                start_events = [event for event in events if event['event'] == 'start']
                starts = {event['stage']: event['at'] for event in start_events}
                requests.append(len(start_events))
                expected = 1 if case == 'search_words' else 2 if case in ('lp_words', 'slow_search') else 3
                if len(start_events) != expected:
                    raise AssertionError(f'{case}: {len(start_events)} requests, expected {expected}')
                if args.enforce and case == 'lp_words' and starts['lp'] - starts['search'] >= 0.14:
                    raise AssertionError('LyricsPlus still waits for the full stagger after a line-only reply')
                if args.enforce and case == 'missing_search' and starts['get'] >= starts['lp'] + 0.5:
                    raise AssertionError('fallback still waits for the slow provider')
                before = log.read_bytes()
                warm.append(invoke(binary, case, env)[0])
                if log.read_bytes() != before:
                    raise AssertionError(f'{case}: warm lookup sent another request')
        reports.append(dict(case=case, cold_ms=round(statistics.median(cold), 2),
                            cold_range_ms=[round(min(cold), 2), round(max(cold), 2)],
                            warm_ms=round(statistics.median(warm), 2),
                            first_result_ms=round(statistics.median(first), 2) if first else None,
                            requests=requests[0]))
    with tempfile.TemporaryDirectory(prefix='veyra-timing-check-') as directory:
        path = Path(directory) / 'fixture.lrc'
        path.write_text('[offset:1000]\n[00:02]<00:02>One <00:03>two\n[00:04]<00:04>Three <00:09>four\n[00:06]Next line')
        data = json.loads(subprocess.check_output([str(binary), 'lyrics-check', str(path), '--duration', '20', '--summary'], text=True, timeout=15))
        if data['invalid_lines'] != 1 or data['word_lines'] != 1 or 'intervals' in data:
            raise AssertionError('local checker: invalid timing/summary regression')
        path.write_text(''.join(f'[{i // 60:02}:{i % 60:02}]<{i // 60:02}:{i % 60:02}>One <{i // 60:02}:{i % 60 + 0.5:06.3f}>two\n' for i in range(8000)))
        local = []
        local_summary = []
        for _ in range(args.runs):
            started = time.perf_counter()
            subprocess.run([str(binary), 'lyrics-check', str(path), '--duration', '8001'], stdout=subprocess.DEVNULL, check=True, timeout=15)
            local.append((time.perf_counter() - started) * 1000)
            started = time.perf_counter()
            data = json.loads(subprocess.check_output([str(binary), 'lyrics-check', str(path), '--duration', '8001', '--summary'], text=True, timeout=15))
            local_summary.append((time.perf_counter() - started) * 1000)
            if data['word_lines'] != 8000 or data['quality'] != 3:
                raise AssertionError('large-file classification regression')
    print(json.dumps(dict(reference='local curl fixtures; no DNS, TLS or internet', runs=args.runs, results=reports,
                          local_8000_lines_ms=round(statistics.median(local), 2),
                          local_summary_8000_lines_ms=round(statistics.median(local_summary), 2)), indent=2))


if __name__ == '__main__':
    main()
