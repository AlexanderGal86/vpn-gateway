# Push VPN Gateway to GitHub

$ErrorActionPreference = "Stop"

# Check gh auth
Write-Host "Checking GitHub authentication..."
gh auth status 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Error: Not authenticated with GitHub. Run: gh auth login"
    exit 1
}

# Check git initialized
if (-not (Test-Path ".git")) {
    Write-Host "Initializing git repository..."
    git init
}

# Set remote
Write-Host "Setting up remote..."
git remote add origin https://github.com/AlexanderGal86/vpn-gateway.git 2>$null

# Stage all files
Write-Host "Staging files..."
git add -A

# Check what will be committed
Write-Host "Files to be committed:"
git status --short

# Create commit if there are changes
$status = git status --porcelain
if ($status) {
    Write-Host "Creating commit..."
    git commit -m "Initial commit: VPN Gateway with proxy pool, WireGuard, and DNS protection"
} else {
    Write-Host "No changes to commit"
}

# Push to GitHub
Write-Host "Pushing to GitHub..."
git push -u origin master:main --force

Write-Host "Done! Repository pushed to https://github.com/AlexanderGal86/vpn-gateway"