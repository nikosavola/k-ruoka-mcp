# Development commands. `just` with no arguments lists them.
#
# Two things here can touch real state, and both say so: `login` writes a live K-Plussa
# session into the browser profile, and `test-account` briefly mutates the real cart.
# Everything else is hermetic or works against a scratch profile.

# Default to listing available recipes
default:
    @just --list

cpus := num_cpus()

# The binary every recipe below drives. Release, because that is what an MCP client runs.
bin := "./target/release/k-ruoka-mcp"

# --- Setup ---

# Install the git hooks and fetch the Rust dependencies
[group('setup')]
install:
    @uv tool install prek
    @prek install
    @cargo fetch

# Remove build output, wheels and the scratch profiles the live suites leave behind
[confirm("Remove target/, wheels and the scratch profiles? (the real login is NOT touched)")]
[group('setup')]
clean:
    @rm -rf target dist
    @rm -rf /tmp/k-ruoka-e2e-profile /tmp/k-ruoka-probe-profile /tmp/k-ruoka-spike-profile

# --- Build ---

# Compile the release binary
[group('build')]
build:
    cargo build --release

# Compile the debug binary
[group('build')]
build-debug:
    cargo build

# --- Linting ---

# Run every git hook over every file
[group('lint')]
pre-commit:
    @uvx prek run --all-files

# Update the pinned hook revisions
[group('lint')]
update-pre:
    @uvx prek autoupdate -j $(( {{ cpus }} / 2 + {{ cpus }} % 2 ))

# Format the Rust and the justfile
[group('lint')]
fmt:
    @cargo fmt --all
    @just --fmt --unstable

# Clippy over the tests and helper binaries too, warnings as errors
[group('lint')]
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# --- Testing ---

# The hermetic suite: no network, no Chrome
[group('test')]
test *args:
    cargo test --all-targets {{ args }}

# The live suite against a scratch profile. Anonymous basket, so no account is touched
[group('test')]
test-live: build-debug
    cargo test --test live_e2e -- --ignored --nocapture

# The account suite. Uses the REAL login and briefly adds then removes one cart item
[confirm("This uses your real K-Plussa login and briefly mutates your REAL cart. Continue?")]
[group('test')]
test-account: build-debug
    K_RUOKA_ACCOUNT_TEST=1 cargo test --test account_e2e -- --ignored --nocapture

# Everything that does not need the real account
[group('test')]
test-all: test test-live

# --- Coverage ---
#
# Source-based coverage via cargo-llvm-cov. Over the hermetic suite only: the live suites
# need network, Chrome and (for one) a real login, so including them would make the number
# depend on the machine.
#
# `spike` and `probe` are excluded because they are development tools kept out of the
# wheel, exactly like [[tool.maturin.targets]] does for the build. Measuring them would
# drag the figure down without saying anything about what ships.
#
# Expect roughly 60% and do not chase 100%. The cart logic, tool surface and wire types
# sit at 95%+; the rest is browser-driving code that cannot run without Chrome, and
# `login` waits for a human by design.

cov_exclude := 'src/bin/'

# Line-coverage floor. Set below the current figure to catch regression, not to aspire.
cov_min := '55'

# Coverage summary in the terminal
[group('coverage')]
coverage:
    cargo llvm-cov --all-targets --ignore-filename-regex '{{ cov_exclude }}' \
      --fail-under-lines {{ cov_min }} --summary-only

# Cobertura XML, which is what CI uploads
[group('coverage')]
coverage-xml:
    cargo llvm-cov --all-targets --ignore-filename-regex '{{ cov_exclude }}' \
      --fail-under-lines {{ cov_min }} --cobertura --output-path coverage.xml

# Browsable HTML report, then print where it landed
[group('coverage')]
coverage-html:
    cargo llvm-cov --all-targets --ignore-filename-regex '{{ cov_exclude }}' --html
    @echo "open target/llvm-cov/html/index.html"

# --- Packaging ---

# Build the wheel. NOTE: tagged with this machine's glibc, so it is not distributable
[group('package')]
wheel:
    @rm -rf dist
    uvx --from 'maturin>=1.9,<2.0' maturin build --release --out dist

# uvx the built wheel and hold an MCP handshake with it, exactly as CI does
[group('package')]
wheel-smoke: wheel
    python3 .github/workflows/smoke_test.py dist

# Check the wheel's metadata renders on PyPI
[group('package')]
wheel-check: wheel
    uvx --from twine twine check dist/*

# Show what actually ends up inside the wheel
[group('package')]
wheel-contents: wheel
    @python3 -c "import glob, zipfile; w = sorted(glob.glob('dist/*.whl'))[-1]; print(w); [print(f'{i.file_size:>12,}  {i.filename}') for i in zipfile.ZipFile(w).infolist()]"

# --- Running ---

# Sign in to K-Plussa by hand. Writes a live session into the browser profile
[group('run')]
login *args: build
    {{ bin }} login {{ args }}

# Run the MCP server over stdio, the way a client does
[group('run')]
serve: build
    {{ bin }} serve

# Whether the stored session is still signed in
[group('run')]
auth-status: build
    @printf '%s\n%s\n%s\n' \
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"just","version":"0"}}}' \
      '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
      '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"auth_status","arguments":{}}}' \
      | {{ bin }} serve 2>/dev/null \
      | python3 -c "import json,sys; [print(json.loads(l)['result']['structuredContent']['detail']) for l in sys.stdin if json.loads(l).get('id')==2]"

# --- Investigation ---
#
# For re-deriving the API contract when K-Ruoka changes their frontend. Both default to a
# scratch profile, never the real login.

# Call a /kr-api/ endpoint through the same Session the server uses
[group('probe')]
probe *args:
    cargo run --bin probe -- {{ args }}

# Read the active basket for a store (default N137)
[group('probe')]
basket store="N137":
    cargo run --bin probe -- POST /kr-api/basket/active '{"storeId":"{{ store }}"}'

# Re-run the baseline browser and Cloudflare measurements
[group('probe')]
spike *args:
    cargo run --bin spike -- {{ args }}

# The "launched browser generation" line is the evidence that the relaunch happened, not
# the "relaunching" line: that one announced relaunches that never occurred until the
# generation sentinel was fixed.
[doc("Provoke a Cloudflare block on purpose and check the relaunch actually fires")]
[group('probe')]
provoke-block:
    @rm -rf /tmp/k-ruoka-rot-check
    -@K_RUOKA_USER_AGENT='Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/150.0.0.0 Safari/537.36' \
      K_RUOKA_PROFILE=/tmp/k-ruoka-rot-check \
      cargo run --quiet --bin probe -- POST /kr-api/basket/active '{"storeId":"N137"}'
    @test -d /tmp/k-ruoka-rot-check \
      && echo "profile survived the block, as it must" \
      || echo "PROFILE WAS DELETED -- that is a bug, it holds the credential"
