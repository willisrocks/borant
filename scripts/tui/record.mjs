// Video of a TUI in tmux: capture the pane every 200 ms while a scenario runs,
// render each changed frame to PNG (scripts/tui/ansi-html.mjs and headless
// Chrome), and join the frames into an MP4 with ffmpeg, each shown as long as
// it was on screen.
//
//   node record.mjs capture <tmux-socket> <out-dir>   (runs until <out-dir>/stop exists)
//   node record.mjs render <out-dir> <columns> <name>  (writes <out-dir>/<name>.mp4)
import { execFileSync, spawnSync } from 'child_process'
import fs from 'fs'
import os from 'os'
import path from 'path'

const [cmd, a, b, c] = process.argv.slice(2)
const here = path.dirname(new URL(import.meta.url).pathname)
const CHROME = process.env.TUI_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

if (cmd === 'capture') {
  const out = path.join(b, 'frames')
  fs.mkdirSync(out, { recursive: true })
  let last = ''
  let n = 0
  const times = []
  const t0 = Date.now()
  while (!fs.existsSync(path.join(b, 'stop'))) {
    const r = spawnSync('tmux', ['-L', a, 'capture-pane', '-e', '-p', '-t', 'tui'], { encoding: 'utf8' })
    const now = Date.now() - t0
    if (r.status === 0 && r.stdout !== last) {
      last = r.stdout
      const file = String(n++).padStart(5, '0') + '.ansi'
      fs.writeFileSync(path.join(out, file), r.stdout)
      times.push({ file, at: now })
      fs.writeFileSync(path.join(b, 'times.json'), JSON.stringify(times))
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 200)
  }
  times.push({ file: null, at: Date.now() - t0 })
  fs.writeFileSync(path.join(b, 'times.json'), JSON.stringify(times))
} else if (cmd === 'render') {
  const dir = a
  const cols = b
  const name = c || 'video'
  const times = JSON.parse(fs.readFileSync(path.join(dir, 'times.json'), 'utf8'))
  const rows = fs.readFileSync(path.join(dir, 'frames', times[0].file), 'utf8').split('\n').length
  const list = []
  for (let i = 0; i < times.length - 1; i++) {
    const f = path.join(dir, 'frames', times[i].file)
    const png = f.replace(/\.ansi$/, '.png')
    if (!fs.existsSync(png)) {
      const txt = f.replace(/\.ansi$/, '.txt')
      fs.copyFileSync(f, txt)
      execFileSync('node', [path.join(here, 'ansi-html.mjs'), txt, txt.replace(/\.txt$/, '.html'), cols])
      const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'rec-'))
      spawnSync(CHROME, ['--headless=new', '--disable-gpu', '--hide-scrollbars', '--no-first-run', '--user-data-dir=' + profile, `--window-size=${Math.round((cols * 843) / 100 + 30)},${rows * 17 + 24}`, '--screenshot=' + png, 'file://' + txt.replace(/\.txt$/, '.html')], { stdio: 'ignore' })
      fs.rmSync(profile, { recursive: true, force: true })
    }
    const dur = Math.max(0.2, (times[i + 1].at - times[i].at) / 1000)
    list.push(`file '${png}'\nduration ${dur.toFixed(2)}`)
  }
  list.push(`file '${path.join(dir, 'frames', times[times.length - 2].file).replace(/\.ansi$/, '.png')}'`)
  fs.writeFileSync(path.join(dir, 'concat.txt'), list.join('\n') + '\n')
  execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'concat', '-safe', '0', '-i', path.join(dir, 'concat.txt'), '-vf', 'scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p', '-r', '10', path.join(dir, name + '.mp4')])
  console.log(path.join(dir, name + '.mp4') + ' · ' + (times.length - 1) + ' frames')
} else {
  console.error('usage: record.mjs capture <socket> <out> | render <out> <columns> <name>')
  process.exit(2)
}
