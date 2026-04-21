# herbalist-mcp installer — Windows
# Usage: irm https://raw.githubusercontent.com/golbinski/herbalist-mcp/main/install.ps1 | iex
#Requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo     = 'golbinski/herbalist-mcp'
$Artifact = 'herbalist-mcp-windows-x86_64.exe'
$Binary   = 'herbalist-mcp.exe'

# ── helpers ───────────────────────────────────────────────────────────────────

function Write-Bold([string]$msg)  { Write-Host $msg -ForegroundColor Cyan }
function Write-Ok([string]$msg)    { Write-Host $msg -ForegroundColor Green }
function Write-Warn([string]$msg)  { Write-Warning $msg }

function Get-LatestVersion {
    $release = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    return $release.tag_name.TrimStart('v')
}

# ── download and verify ───────────────────────────────────────────────────────

function Install-Binary {
    $version = Get-LatestVersion
    $baseUrl = "https://github.com/$Repo/releases/download/v$version"
    $tmpExe  = Join-Path $env:TEMP $Artifact
    $tmpSha  = Join-Path $env:TEMP "${Artifact}.sha256"

    Write-Bold "Downloading $Artifact v$version..."
    Invoke-WebRequest "$baseUrl/$Artifact"               -OutFile $tmpExe
    Invoke-WebRequest "$baseUrl/${Artifact}.sha256"      -OutFile $tmpSha

    $expected = ((Get-Content $tmpSha) -split '\s+')[0].ToUpper()
    $actual   = (Get-FileHash $tmpExe -Algorithm SHA256).Hash.ToUpper()
    if ($expected -ne $actual) {
        throw "SHA256 mismatch!`n  expected: $expected`n  actual:   $actual"
    }
    Write-Ok "Checksum verified."
    Remove-Item $tmpSha

    $installDir = Join-Path $env:LOCALAPPDATA 'herbalist-mcp'
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $dest = Join-Path $installDir $Binary
    Move-Item -Force $tmpExe $dest

    # Add install dir to user PATH if not already present
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if ($null -eq $userPath) { $userPath = '' }
    if ($userPath -notlike "*$installDir*") {
        [Environment]::SetEnvironmentVariable('PATH', "$userPath;$installDir", 'User')
        Write-Warn "Added $installDir to user PATH. Restart your shell for it to take effect."
    }

    Write-Ok "Installed to $dest"
    return $dest
}

# ── JSON config helpers ───────────────────────────────────────────────────────

# Merge-Json works on PS 5.1 by converting PSCustomObject to a nested hashtable.
function ConvertTo-DeepHashtable($obj) {
    if ($obj -is [System.Management.Automation.PSCustomObject]) {
        $ht = [ordered]@{}
        foreach ($prop in $obj.PSObject.Properties) {
            $ht[$prop.Name] = ConvertTo-DeepHashtable $prop.Value
        }
        return $ht
    }
    if ($obj -is [object[]]) {
        return @($obj | ForEach-Object { ConvertTo-DeepHashtable $_ })
    }
    return $obj
}

function Read-JsonConfig([string]$path) {
    if (Test-Path $path) {
        try { return ConvertTo-DeepHashtable (Get-Content $path -Raw | ConvertFrom-Json) }
        catch { }
    }
    return [ordered]@{}
}

function Write-JsonConfig([string]$path, $config) {
    $config | ConvertTo-Json -Depth 10 | Set-Content $path -Encoding UTF8
}

# ── MCP config writers ────────────────────────────────────────────────────────

function Set-ClaudeConfig([string]$binPath, [string]$vault) {
    $configPath  = Join-Path $env:USERPROFILE '.claude.json'
    $claudeFound = Get-Command 'claude' -ErrorAction SilentlyContinue

    if (-not $claudeFound -and -not (Test-Path $configPath)) { return }

    Write-Bold "Configuring Claude Code..."
    $config = Read-JsonConfig $configPath
    if (-not $config.ContainsKey('mcpServers')) { $config['mcpServers'] = [ordered]@{} }
    $config['mcpServers']['herbalist'] = [ordered]@{
        command = $binPath
        args    = @('serve')
        env     = [ordered]@{ HERBALIST_VAULT = $vault; HERBALIST_LOG = 'herbalist_mcp=warn' }
    }
    Write-JsonConfig $configPath $config
    Write-Ok "  -> $configPath"
}

function Set-VSCodeConfig([string]$binPath, [string]$vault) {
    $vscodeDir = Join-Path $env:APPDATA 'Code\User'
    if (-not (Test-Path $vscodeDir)) { return }

    Write-Bold "Configuring VS Code MCP..."
    $configPath = Join-Path $vscodeDir 'mcp.json'
    $config = Read-JsonConfig $configPath
    if (-not $config.ContainsKey('servers')) { $config['servers'] = [ordered]@{} }
    $config['servers']['herbalist'] = [ordered]@{
        type    = 'stdio'
        command = $binPath
        args    = @('serve')
        env     = [ordered]@{ HERBALIST_VAULT = $vault; HERBALIST_LOG = 'herbalist_mcp=warn' }
    }
    Write-JsonConfig $configPath $config
    Write-Ok "  -> $configPath"
}

# ── main ──────────────────────────────────────────────────────────────────────

Write-Bold "herbalist-mcp installer"
Write-Host ""

$binPath = Install-Binary

Write-Host ""
$vault = Read-Host "Vault path (leave blank to configure later)"

if ($vault) {
    Write-Bold "Indexing vault..."
    & $binPath index --vault $vault

    Set-ClaudeConfig $binPath $vault
    Set-VSCodeConfig $binPath $vault
} else {
    Write-Warn "Skipping index and MCP config — no vault path given."
    Write-Host "Run the following when ready:"
    Write-Host "  herbalist-mcp index --vault <path>"
}

Write-Host ""
Write-Ok "Done! Run 'herbalist-mcp --help' to get started."
