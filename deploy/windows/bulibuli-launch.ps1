param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$CorePath,
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)]
    [string[]]$CoreArgs
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location -LiteralPath $Root

if (-not (Test-Path -LiteralPath $CorePath -PathType Leaf)) {
    Write-Error "Core program not found: $CorePath"
    exit 1
}

& $CorePath @CoreArgs
exit $LASTEXITCODE
