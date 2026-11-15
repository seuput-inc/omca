$ErrorActionPreference = 'Stop'
try {
    $owner = Get-Process -Id ([int]$env:OMCA_OWNER_PID) -ErrorAction Stop
} catch {
    if ($_.FullyQualifiedErrorId.Split(',')[0] -eq 'NoProcessFoundForGivenId') {
        exit 0
    }
    throw
}
[Console]::Write($owner.StartTime.ToUniversalTime().Ticks.ToString() + '|' + $owner.Path)
exit 0
