[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$ExecutablePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

try {
  # Remove only the old unbounded helper log. Keep the shared deployment log
  # directory and every other file for the administrator.
  $legacyLaunchLog = Join-Path $env:ProgramData 'HualiAI\Logs\launch-after-install.log'
  if (Test-Path -LiteralPath $legacyLaunchLog -PathType Leaf) {
    Remove-Item -LiteralPath $legacyLaunchLog -Force -ErrorAction SilentlyContinue
  }
} catch {
  # Cleanup must not delay or prevent an interactive launch.
}

try {
  if (-not (Test-Path -LiteralPath $ExecutablePath)) {
    exit 0
  }

  $interactiveUsers = @{}
  Get-CimInstance Win32_Process -Filter "Name='explorer.exe'" -ErrorAction SilentlyContinue | ForEach-Object {
    $owner = Invoke-CimMethod -InputObject $_ -MethodName GetOwner -ErrorAction SilentlyContinue
    if ($owner -and ($owner.ReturnValue -eq 0) -and $owner.User) {
      $account = if ($owner.Domain) { "$($owner.Domain)\$($owner.User)" } else { $owner.User }
      $interactiveUsers[$account] = $true
    }
  }

  if ($interactiveUsers.Count -eq 0) {
    exit 0
  }

  foreach ($account in $interactiveUsers.Keys) {
    $taskName = "HualiAI-Launch-$([Guid]::NewGuid().ToString('N'))"
    try {
      $action = New-ScheduledTaskAction `
        -Execute $ExecutablePath `
        -WorkingDirectory (Split-Path -Parent $ExecutablePath)
      $principal = New-ScheduledTaskPrincipal `
        -UserId $account `
        -LogonType Interactive `
        -RunLevel Limited
      $task = New-ScheduledTask -Action $action -Principal $principal
      Register-ScheduledTask -TaskName $taskName -InputObject $task -Force | Out-Null
      Start-ScheduledTask -TaskName $taskName
      Start-Sleep -Milliseconds 1500
    } catch {
      # Keep installation successful; HKLM Run retries at the next user logon.
    } finally {
      Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
  }
} catch {
  # An unavailable interactive desktop must not fail the MSI installation.
}

exit 0
