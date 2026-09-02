$ErrorActionPreference = "Stop"
$Repo = "PerceivingAI/portus-window"
$InstallDir = "$env:LOCALAPPDATA\Programs\PortusWindow"
$SkillsDir = "$env:USERPROFILE\.claude\skills\portus-window"
$AgentsSkillsDir = "$env:USERPROFILE\.agents\skills\portus-window"
$CodexSkillsDir = "$env:USERPROFILE\.codex\skills\portus-window"

Write-Host "==> Installing Portus Window for Windows..." -ForegroundColor Cyan

$ReleaseApi = "https://api.github.com/repos/$Repo/releases/latest"
$Tag = "latest"
try {
    $Release = Invoke-RestMethod -Uri $ReleaseApi -UseBasicParsing
    if ($Release.tag_name) { $Tag = $Release.tag_name }
} catch {
    Write-Host "Note: Falling back to latest release tag"
}

$ZipName = "portus-window-windows-x86_64.zip"
$DownloadUrl = "https://github.com/$Repo/releases/download/$Tag/$ZipName"
$TempZip = "$env:TEMP\portus-window.zip"

Write-Host "==> Downloading $ZipName ($Tag)..."
try {
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TempZip -UseBasicParsing
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Expand-Archive -Path $TempZip -DestinationPath $InstallDir -Force
    Remove-Item -Path $TempZip -Force
} catch {
    throw "Release binary download failed: $($_.Exception.Message)"
}

# Update User PATH
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    $env:PATH += ";$InstallDir"
    Write-Host "✓ Added $InstallDir to User PATH" -ForegroundColor Green
}

# Install Agent Skill
if (Test-Path "$InstallDir\skills\portus-window") {
    New-Item -ItemType Directory -Force -Path $SkillsDir | Out-Null
    New-Item -ItemType Directory -Force -Path $AgentsSkillsDir | Out-Null
    New-Item -ItemType Directory -Force -Path $CodexSkillsDir | Out-Null
    Copy-Item -Path "$InstallDir\skills\portus-window\*" -Destination $SkillsDir -Recurse -Force
    Copy-Item -Path "$InstallDir\skills\portus-window\*" -Destination $AgentsSkillsDir -Recurse -Force
    Copy-Item -Path "$InstallDir\skills\portus-window\*" -Destination $CodexSkillsDir -Recurse -Force
    Write-Host "✓ Agent skill installed to $SkillsDir" -ForegroundColor Green
}

Write-Host "`n✓ Portus Window installation complete!" -ForegroundColor Green
Write-Host "Restart your terminal or agent session to use 'portus-window-cli'."
