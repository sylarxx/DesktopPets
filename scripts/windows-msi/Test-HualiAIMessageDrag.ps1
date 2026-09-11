[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][int]$ApplicationProcessId,
  [Parameter(Mandatory = $true)][string]$OutputDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class HualiMessageDragNative {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public class Window { public long Handle; public int Left, Top, Width, Height; public uint Dpi; }
  private delegate bool Callback(IntPtr window, IntPtr parameter);
  [DllImport("user32.dll")] private static extern bool EnumWindows(Callback callback, IntPtr parameter);
  [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
  [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint owner);
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr window, out RECT rect);
  [DllImport("user32.dll")] private static extern uint GetDpiForWindow(IntPtr window);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
  public static Window[] Snapshot(int processId) {
    var list = new List<Window>();
    EnumWindows((window, parameter) => {
      uint owner; RECT rect;
      GetWindowThreadProcessId(window, out owner);
      if (owner == processId && IsWindowVisible(window) && GetWindowRect(window, out rect)) {
        list.Add(new Window { Handle = window.ToInt64(), Left = rect.Left, Top = rect.Top,
          Width = rect.Right - rect.Left, Height = rect.Bottom - rect.Top, Dpi = GetDpiForWindow(window) });
      }
      return true;
    }, IntPtr.Zero);
    return list.ToArray();
  }
}
'@
[HualiMessageDragNative]::SetThreadDpiAwarenessContext([IntPtr](-4)) | Out-Null
function Save-DragScreen([string]$Name) {
  $bounds = [Windows.Forms.SystemInformation]::VirtualScreen
  $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
  $graphics = [Drawing.Graphics]::FromImage($bitmap)
  try {
    $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bitmap.Size)
    $bitmap.Save((Join-Path $OutputDirectory $Name), [Drawing.Imaging.ImageFormat]::Png)
  } finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Get-DragWindows { return @([HualiMessageDragNative]::Snapshot($ApplicationProcessId)) }
$report = [ordered]@{ ok = $false; failure = $null; samples = @(); scope = 'real mouse drag of authenticated system message card' }
$mouseDown = $false
try {
  $readyDeadline = [DateTime]::UtcNow.AddSeconds(10)
  do {
  $before = Get-DragWindows
  $mascot = $before | Where-Object {
    $scale = [Math]::Max(96, $_.Dpi) / 96.0
    [Math]::Abs($_.Width / $scale - 120) -le 3 -and [Math]::Abs($_.Height / $scale - 104) -le 3
  } | Select-Object -First 1
  $card = $before | Where-Object {
    $scale = [Math]::Max(96, $_.Dpi) / 96.0
    [Math]::Abs($_.Width / $scale - 320) -le 3 -and $_.Height / $scale -gt 200
  } | Select-Object -First 1
  if ($mascot -and $card) { break }
  Start-Sleep -Milliseconds 100
  } while ([DateTime]::UtcNow -lt $readyDeadline)
  if (-not $mascot -or -not $card) { throw '未找到真实机器人及系统消息卡片窗口，不能执行拖动验收。' }
  $report.before = [ordered]@{ mascot = $mascot; card = $card }
  Save-DragScreen 'message-before-drag.png'
  $startX = $mascot.Left + [int]($mascot.Width / 2)
  $startY = $mascot.Top + [int]($mascot.Height / 2)
  [HualiMessageDragNative]::SetCursorPos($startX, $startY) | Out-Null
  Start-Sleep -Milliseconds 150
  [HualiMessageDragNative]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
  $mouseDown = $true
  for ($step = 1; $step -le 12; $step++) {
    [HualiMessageDragNative]::SetCursorPos(($startX - $step * 12), ($startY - $step * 4)) | Out-Null
    Start-Sleep -Milliseconds 70
    $windows = Get-DragWindows
    $visibleCard = $windows | Where-Object { $_.Handle -eq $card.Handle } | Select-Object -First 1
    $visibleMascot = $windows | Where-Object { $_.Handle -eq $mascot.Handle } | Select-Object -First 1
    if (-not $visibleCard -or -not $visibleMascot) { throw "拖动第 $step 步时原卡片或机器人窗口消失。" }
    $report.samples += [ordered]@{ step = $step; mascot = $visibleMascot; card = $visibleCard }
    if ($step -eq 6) { Save-DragScreen 'message-during-drag.png' }
  }
  [HualiMessageDragNative]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
  $mouseDown = $false
  Start-Sleep -Milliseconds 450
  $after = Get-DragWindows
  $afterMascot = $after | Where-Object { $_.Handle -eq $mascot.Handle } | Select-Object -First 1
  $afterCard = $after | Where-Object { $_.Handle -eq $card.Handle } | Select-Object -First 1
  if (-not $afterMascot -or -not $afterCard) { throw '松手后原卡片或机器人窗口消失。' }
  if ([Math]::Abs($afterMascot.Left - $mascot.Left) -lt 60) { throw '真实鼠标操作没有形成有效拖动。' }
  if ([Math]::Abs($afterCard.Left - $card.Left) + [Math]::Abs($afterCard.Top - $card.Top) -lt 30) { throw '消息卡片没有跟随机器人移动。' }
  $overlaps = $afterCard.Left -lt ($afterMascot.Left + $afterMascot.Width) -and
    ($afterCard.Left + $afterCard.Width) -gt $afterMascot.Left -and
    $afterCard.Top -lt ($afterMascot.Top + $afterMascot.Height) -and
    ($afterCard.Top + $afterCard.Height) -gt $afterMascot.Top
  if ($overlaps) { throw '松手后消息卡片覆盖机器人窗口。' }
  $report.after = [ordered]@{ mascot = $afterMascot; card = $afterCard; sameWindows = $true; noOverlap = $true }
  Save-DragScreen 'message-after-drag.png'
  $report.ok = $true
} catch {
  $report.failure = $_.Exception.Message
  try { Save-DragScreen 'message-drag-failure.png' } catch {}
  throw
} finally {
  if ($mouseDown) { [HualiMessageDragNative]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero) }
  $report | ConvertTo-Json -Depth 10 | Set-Content (Join-Path $OutputDirectory 'message-drag-report.json') -Encoding UTF8
}
