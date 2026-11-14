# claude-code-alab

`claude-alab` runs [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
and keeps it current while you work. When a new `claude` build is installed, the
running session moves onto it at the next quiet moment and picks up the same
conversation. Nothing to restart, nothing to retype.

It runs the unmodified vendor `claude` from the atpkg store. It never ships,
forks or patches vendor code.

```text
$ claude-alab                      # same as `claude`, kept up to date
$ claude-alab --model opus         # every argument goes to `claude` unchanged
$ claude-alab --version
claude-code-alab <version> (claude <vendor version>)
```

Install Claude Code with `aterm pkg install claude`. Without it, `claude-alab`
says so and exits 127.

## What it does

**Runs Claude Code as its child on your terminal.** Arguments, environment,
working directory and signals pass straight through, and `claude-alab` exits
exactly the way `claude` did.

**Steps aside when it has nothing to add.** The harness stays only with a
session it can move onto another build: one on a terminal, started with
arguments it can resume. Any other launch runs `claude` in the harness's own
place, as the same process, with nothing in between: no terminal on stdin, a
one-shot run (`-p`, `--help`, `--version`), or flags that have no resume form
(`--worktree`, `--tmux`, a flag the harness does not know).

**Updates Claude Code under the running session.** `aterm pkg update claude`
(or a rollback) changes which build is current. The harness notices, waits
until the session is idle, nothing is typed and no background work is running,
then ends the old build with `SIGTERM` and starts the new one with
`--resume <conversation>`. Launch flags and the session's permission mode are
carried over. If the new build will not start, the session resumes on the old
one. A session that is busy is never interrupted.

**Updates itself.** The harness is a package too. When a new `claude-alab`
build is installed, the running harness replaces its own program image in
place and keeps the same `claude` child, so the session does not notice. If
the new image cannot take over, it returns to the old one.

**Works with aterm.** Inside an aterm tab the harness claims aterm's supervisor
contract and uses aterm's own view of the session (is the agent idle, is the
composer empty, did a person just type) to pick the moment to update. Under
that claim it also brings `claude` back on the same conversation if it
crashes, and restarts it when aterm asks. Without the claim it uses its own
readings of the terminal and Claude Code's session record, and behaves the
same anywhere else.

## Options

Options for the harness come first and start with `--alab-`. Everything after
them is passed to `claude`.

| Option | Effect | Default |
|---|---|---|
| `--alab-no-live-update` | Never move this session: run `claude` alone, in the harness's place | |
| `--alab-quiet-ms=<ms>` | How long the session must be idle before an update | 20000 |
| `--alab-poll-ms=<ms>` | How often the store and the session are checked | 250 |
| `--alab-term-grace-ms=<ms>` | How long an update waits for `claude` to end | 30000 |
| `--alab-help` | Show the usage text | |

atpkg's `claude` hands a session over as `claude-alab run -- <arguments>`.
Inside aterm, `ATERM_NO_HARNESS=1` runs `claude` alone too.

## The journal

Every decision the harness makes is one line in
`~/.local/state/claude-code-alab/journal.log` (or under `$XDG_STATE_HOME`):
each start, each update it saw, why it is waiting, each relaunch, each
refusal. When an update is not happening, the reason is the last `wait` line.
A launch that ran `claude` alone is one line, `pass <reason>`.

## Status

Working today:

* running, signal handling and exit status
* live update of Claude Code with fallback to the previous build
* self-update outside aterm
* the aterm claim, and the update moment taken from aterm, tested against a
  stand-in for aterm
* under that claim, relaunching a crashed `claude` on the same conversation
  and restarting it when aterm asks, tested against the same stand-in

Not done yet:

* **aterm does not list this harness yet**, so a real aterm session runs on the
  harness's own readings. The aterm change is in progress.
* **A crash is relaunched only under the aterm claim.** Without it, which
  today is every real session, the harness exits with `claude`'s status.
* **Self-update inside aterm** waits on the claim above.
* **A model changed with `/model`** comes back as the launch flags set it.
* **Linux** builds and follows the same rules but has not been run against a
  real Claude Code. macOS is the measured platform.

Where this is going: aterm drawing the session natively instead of as a grid of
terminal characters, and the harness's custody of the session moving into
shared ALab system services.

## Building

The repository pins the Trust toolchain (`rust-toolchain.toml`).

```sh
targo --unverified build --locked
targo --unverified test --locked
targo tippy --locked --all-targets -- -D warnings
targo fmt --check
```

The tests run the real binary on a real pseudo-terminal against a scripted
stand-in for Claude Code, and against a stand-in for aterm's control socket.

## License

Apache-2.0; see [LICENSE](LICENSE).
