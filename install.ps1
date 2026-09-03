# Herdr Agent Quota Plugin - Windows Installation Script
# PowerShell version of install.sh

param(
    [switch]$Help,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

# Colors
$RED = "`e[31m"
$GREEN = "`e[32m"
$YELLOW = "`e[33m"
$BLUE = "`e[34m"
$RESET = "`e[0m"

function Write-ColorOutput {
    param([string]$Color, [string]$Message)
    Write-Host "$Color$Message$RESET"
}

function Show-Help {
    Write-Host @"
Install the herdr-agent-quota plugin.

Usage: .\install.ps1 [OPTIONS]

Options:
    -Help       Show this help message
    -Uninstall  Uninstall the plugin and restore original settings

Examples:
    .\install.ps1              # Install the plugin
    .\install.ps1 -Uninstall   # Uninstall the plugin
"@
    exit 0
}

if ($Help) {
    Show-Help
}

# Get script directory
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path

# Check prerequisites
function Test-Command {
    param([string]$Command)
    $null -ne (Get-Command $Command -ErrorAction SilentlyContinue)
}

if (-not (Test-Command "herdr")) {
    Write-ColorOutput $RED "Error: herdr not found in PATH"
    Write-Host "Please install Herdr first: https://herdr.dev"
    exit 1
}

if (-not (Test-Command "cargo")) {
    Write-ColorOutput $RED "Error: cargo not found in PATH"
    Write-Host "Please install Rust: https://rustup.rs"
    exit 1
}

# Build the plugin
Write-ColorOutput $BLUE "Building plugin..."
Push-Location $ROOT
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) {
        Write-ColorOutput $RED "Build failed"
        exit 1
    }
} finally {
    Pop-Location
}

# Get plugin config directory
$CONFIG_DIR = (herdr plugin config-dir herdr-agent-quota | Out-String).Trim()
if (-not $CONFIG_DIR) {
    Write-ColorOutput $RED "Failed to get plugin config directory"
    exit 1
}

$AGENTS_FILE = Join-Path $CONFIG_DIR "agents.json"
$BACKUP_FILE = Join-Path $CONFIG_DIR "agents.backup.json"

# Create config directory
if (-not (Test-Path $CONFIG_DIR)) {
    New-Item -ItemType Directory -Path $CONFIG_DIR -Force | Out-Null
}

if ($Uninstall) {
    Write-ColorOutput $BLUE "Uninstalling plugin..."

    # Disable plugin
    herdr plugin disable herdr-agent-quota 2>&1 | Out-Null

    # Restore backup if exists
    if (Test-Path $BACKUP_FILE) {
        Copy-Item $BACKUP_FILE $AGENTS_FILE -Force
        Remove-Item $BACKUP_FILE -Force
        Write-ColorOutput $GREEN "Restored original agents preferences"
    } else {
        # Remove the file if no backup exists
        if (Test-Path $AGENTS_FILE) {
            Remove-Item $AGENTS_FILE -Force
        }
    }

    # Unlink plugin
    herdr plugin unlink herdr-agent-quota

    Write-ColorOutput $GREEN "Plugin uninstalled successfully"
    exit 0
}

# Install
Write-ColorOutput $BLUE "Installing plugin..."

# Link plugin
herdr plugin link "$ROOT"
if ($LASTEXITCODE -ne 0) {
    Write-ColorOutput $RED "Failed to link plugin"
    exit 1
}

# Enable plugin
herdr plugin enable herdr-agent-quota
if ($LASTEXITCODE -ne 0) {
    Write-ColorOutput $RED "Failed to enable plugin"
    exit 1
}

# Backup existing agents.json
if ((Test-Path $AGENTS_FILE) -and -not (Test-Path $BACKUP_FILE)) {
    Copy-Item $AGENTS_FILE $BACKUP_FILE -Force
    Write-ColorOutput $YELLOW "Backed up existing agents.json"
}

# Create default agents.json with all providers enabled
$DEFAULT_AGENTS = @"
{
  "herdr-agent-quota": {
    "agents": ["codex", "grok", "agy", "opencode", "pi", "omp"]
  }
}
"@

Set-Content -Path $AGENTS_FILE -Value $DEFAULT_AGENTS -Encoding UTF8
Write-ColorOutput $GREEN "Created default agents.json with all providers"

# Invoke refresh action
Write-ColorOutput $BLUE "Refreshing quota data..."
$ACTION_ID = (herdr action invoke --json herdr-agent-quota refresh | ConvertFrom-Json).actionId

if ($ACTION_ID) {
    # Poll for completion
    $MAX_WAIT = 30
    $WAITED = 0

    while ($WAITED -lt $MAX_WAIT) {
        Start-Sleep -Seconds 1
        $WAITED++

        $LOGS = herdr plugin log list --json herdr-agent-quota | ConvertFrom-Json
        $LOG = $LOGS | Where-Object { $_.actionId -eq $ACTION_ID } | Select-Object -First 1

        if ($LOG -and $LOG.status -ne "running") {
            if ($LOG.status -eq "success") {
                Write-ColorOutput $GREEN "✓ Initial refresh completed"
            } else {
                Write-ColorOutput $YELLOW "! Initial refresh had issues (this is normal on first run)"
            }
            break
        }
    }

    if ($WAITED -ge $MAX_WAIT) {
        Write-ColorOutput $YELLOW "! Initial refresh is still running (this is normal)"
    }
} else {
    Write-ColorOutput $YELLOW "! Could not trigger initial refresh"
}

Write-ColorOutput $GREEN "`n✓ Installation complete!"
Write-Host ""
Write-Host "The quota plugin is now active in your Herdr statusline."
Write-Host "It will refresh automatically, or run: ${BLUE}herdr action invoke herdr-agent-quota refresh$RESET"
Write-Host ""
Write-Host "To customize which agents to monitor, edit:"
Write-Host "  $AGENTS_FILE"
Write-Host ""
Write-Host "To uninstall: ${BLUE}.\install.ps1 -Uninstall$RESET"
