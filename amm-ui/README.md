# AMM UI

A QML UI application for the Automated Market Maker (AMM) program.

See the [Logos QML UI App Tutorial](https://github.com/logos-co/logos-tutorial/blob/master/tutorial-qml-ui-app.md) for more information.

## Setup

This project requires Nix with experimental features enabled. If you haven't already, enable them permanently:

```bash
mkdir -p ~/.config/nix && echo "experimental-features = nix-command flakes" >> ~/.config/nix/nix.conf
```

Install the Logos package manager CLI globally (one-time):

```bash
nix profile install 'github:logos-co/logos-package-manager#cli'
```

This makes `lgpm` available as a global command.

## Running the UI standalone

Start the UI with:

```bash
nix run .
```

This builds and runs the application in development mode. The Logos bridge is unavailable in standalone mode, but the UI layout and mock data are fully functional.

## Running inside Logos Basecamp

### 1. Build the LGX package

```bash
# Development variant (requires nix store at runtime)
nix build '.#lgx' --out-link result-lgx

# Portable variant (self-contained, works without nix)
nix build '.#lgx-portable' --out-link result-lgx-portable
```

### 2. Install into Basecamp

```bash
# Launch Basecamp once to initialise its data directory, then quit
nix run 'github:logos-co/logos-basecamp'

# Set the data directory path
# macOS:
BASECAMP_DIR="$HOME/Library/Application Support/Logos/LogosBasecampDev"
# Linux:
# BASECAMP_DIR="$HOME/.local/share/Logos/LogosBasecampDev"

# Install the UI plugin
lgpm --ui-plugins-dir "$BASECAMP_DIR/plugins" \
  install --file result-lgx/*.lgx
```

> **Note:** Use matching variants throughout — dev with dev, portable with portable. Mixing variants causes loading failures. The portable build uses the `LogosBasecamp` data directory instead of `LogosBasecampDev`.

### 3. Launch Basecamp

```bash
nix run 'github:logos-co/logos-basecamp'
```

This fetches, builds, and launches Basecamp in one step. The AMM UI appears as a new tab in the Basecamp sidebar.

### Installing via the Basecamp UI

Alternatively, use the built-in package manager:

1. Launch Basecamp
2. Open Package Manager
3. Select "Install from file"
4. Choose the `.lgx` file from `result-lgx/`

## Updating Dependencies

To update the pinned versions of dependencies in `flake.lock`:

```bash
nix flake update
```

## Troubleshooting

**Stale QML cache after rebuild:**
```bash
QML_DISABLE_DISK_CACHE=1 nix run 'github:logos-co/logos-basecamp'
```

**Reset Basecamp data directory:**
```bash
# macOS
rm -rf ~/Library/Application\ Support/Logos/LogosBasecampDev
# Linux
rm -rf ~/.local/share/Logos/LogosBasecampDev
```
