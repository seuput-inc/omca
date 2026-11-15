$ErrorActionPreference = 'Stop'
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($env:OMCA_UPDATE_MSI, 0)
function Read-Property([string] $name) {
    $query = 'SELECT `Value` FROM `Property` WHERE `Property` = ''' + $name + ''''
    $view = $database.OpenView($query)
    $view.Execute()
    return $view.Fetch().StringData(1)
}
if ((Read-Property 'UpgradeCode') -ne '{A2D8C48E-578F-495D-ACB0-D9F85CCB26F6}' -or
    (Read-Property 'ProductName') -ne 'OMCA' -or
    (Read-Property 'ProductVersion') -ne $env:OMCA_UPDATE_VERSION) {
    throw 'Wrong OMCA installer identity'
}
if ($env:OMCA_UPDATE_MSI.Contains('"') -or $env:OMCA_UPDATE_ROOT.Contains('"')) {
    throw 'Invalid OMCA installer path'
}
$arguments = '/i "' + $env:OMCA_UPDATE_MSI + '" /passive /norestart INSTALLFOLDER="' + $env:OMCA_UPDATE_ROOT + '"'
$process = Start-Process -FilePath ($env:SystemRoot + '\System32\msiexec.exe') -ArgumentList $arguments -Verb RunAs -PassThru
# Exit a distinct code on deadline. The unelevated helper must not restart an app while MSI may still write it.
if (-not $process.WaitForExit(840000)) { exit 124 }
if ($process.ExitCode -notin @(0, 3010)) { throw ('OMCA installer failed: ' + $process.ExitCode) }
