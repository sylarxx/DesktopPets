import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import appSource from '../App.vue?raw'
import contextMenuSource from '../components/MascotContextMenu.vue?raw'
import windowServiceSource from '../services/window.service.ts?raw'
import mascotMenuWindowSource from '../views/MascotMenuWindow.vue?raw'
import mascotWindowSource from '../views/MascotWindow.vue?raw'
import capabilitiesSource from '../../src-tauri/capabilities/default.json?raw'
import rustSource from '../../src-tauri/src/main.rs?raw'
import tauriConfigSource from '../../src-tauri/tauri.conf.json?raw'

const appStyles = readFileSync(new URL('../assets/styles/app.css', import.meta.url), 'utf8')

function sourceBetween(source: string, startMarker: string, endMarker: string) {
  const start = source.indexOf(startMarker)
  const end = source.indexOf(endMarker, start + startMarker.length)

  expect(start, `missing source marker: ${startMarker}`).toBeGreaterThanOrEqual(0)
  expect(end, `missing source marker: ${endMarker}`).toBeGreaterThan(start)
  return source.slice(start, end)
}

describe('mascot context menu architecture', () => {
  it('routes the menu through its own transparent native window', () => {
    const tauriConfig = JSON.parse(tauriConfigSource) as {
      app: { windows: Array<Record<string, unknown>> }
    }
    const capabilities = JSON.parse(capabilitiesSource) as { windows: string[] }
    const menuWindow = tauriConfig.app.windows.find((window) => window.label === 'mascot-menu')

    expect(menuWindow).toMatchObject({
      label: 'mascot-menu',
      url: 'index.html?window=mascot-menu',
      width: 216,
      height: 76,
      decorations: false,
      transparent: true,
      backgroundColor: '#00000000',
      shadow: false,
      alwaysOnTop: true,
      visible: false,
      skipTaskbar: true
    })
    expect(capabilities.windows).toContain('mascot-menu')
    expect(appSource).toContain("import MascotMenuWindow from './views/MascotMenuWindow.vue'")
    expect(appSource).toContain("windowMode === 'mascot-menu'")
  })

  it('keeps the placement payload restricted to above or below end to end', () => {
    expect(windowServiceSource).toMatch(
      /interface MascotContextMenuPlacement\s*\{[\s\S]*?generation: number[\s\S]*?placement: 'above' \| 'below'[\s\S]*?tailX: number[\s\S]*?\}/
    )
    expect(mascotMenuWindowSource).toMatch(
      /const placement = ref<'above' \| 'below'>\(/
    )
    expect(mascotMenuWindowSource).toContain(
      "previewPlacement === 'below' ? 'below' : 'above'"
    )
    expect(mascotMenuWindowSource).toMatch(
      /listen<MascotContextMenuPlacement>\(\s*'mascot-context-menu-placement'/
    )
    expect(mascotMenuWindowSource).toContain('placement.value = payload.placement')
    expect(mascotMenuWindowSource).toContain('tailX.value = payload.tailX')
    expect(contextMenuSource).toContain("placement?: 'above' | 'below'")
    expect(contextMenuSource).toContain('tailX?: number')
    expect(contextMenuSource).toContain("placement === 'below' ? 'is-below' : 'is-above'")
    expect(rustSource).toMatch(
      /enum MascotContextMenuPlacement\s*\{\s*Above,\s*Below,\s*\}/
    )
    expect(rustSource).toMatch(
      /struct MascotContextMenuPlacementPayload\s*\{[\s\S]*?generation: u64,[\s\S]*?placement: MascotContextMenuPlacement,[\s\S]*?tail_x: f64,[\s\S]*?\}/
    )
    expect(rustSource).toMatch(
      /emit_to\(\s*"mascot-menu",\s*"mascot-context-menu-placement",\s*geometry\.payload/
    )
  })

  it('does not reintroduce the old inline menu resize state', () => {
    expect(mascotWindowSource).not.toMatch(/import\s+MascotContextMenu\b/)
    expect(mascotWindowSource).not.toContain('<MascotContextMenu')
    expect(mascotWindowSource).not.toContain('getMascotContextMenuLayout')
    expect(mascotWindowSource).not.toContain('isContextMenuPreparing')
    expect(mascotWindowSource).not.toContain('isContextMenuDismissing')
    expect(mascotWindowSource).not.toContain('contextMenu.value')

    const handler = sourceBetween(
      mascotWindowSource,
      'async function handleContextMenu',
      'const hasBubbleMessage'
    )
    expect(handler).toContain('await showMascotContextMenu()')
    expect(handler).not.toContain('isContextMenuVisible.value = true')
    expect(handler).not.toContain('syncNativeNotificationLayout')
    expect(handler).not.toContain('setMascotNotificationVisible')
  })

  it('positions the detached menu from the avatar using physical window bounds', () => {
    const nativeHandler = sourceBetween(
      rustSource,
      'fn prepare_mascot_context_menu_generation',
      'fn schedule_mascot_context_menu_timeout'
    )

    expect(nativeHandler).toContain('mascot_client_origin_physical(')
    expect(nativeHandler).toContain('mascot_avatar_physical_rect(')
    expect(nativeHandler).toContain('mascot_context_menu_physical_geometry(')
    expect(nativeHandler).toContain('set_window_physical_bounds(')
    expect(nativeHandler).toContain('work_area.position.x')
    expect(nativeHandler).toContain('work_area.size.width')
    expect(nativeHandler).not.toContain('menu.show()')
  })

  it('waits for a generation-scoped frontend layout ACK before showing the HWND', () => {
    const nativeAck = sourceBetween(
      rustSource,
      'fn ack_mascot_context_menu_layout',
      'fn hide_mascot_context_menu('
    )

    expect(windowServiceSource).toContain("invoke<boolean>('show_mascot_context_menu')")
    expect(windowServiceSource).toContain("invoke<boolean>('ack_mascot_context_menu_layout', { generation })")
    expect(mascotMenuWindowSource).toContain(':key="menuGeneration"')
    expect(mascotMenuWindowSource).toContain('await nextTick()')
    expect(mascotMenuWindowSource).toContain('menuWindowElement.getBoundingClientRect()')
    expect(mascotMenuWindowSource).not.toContain('window.requestAnimationFrame(')
    expect(mascotMenuWindowSource).toContain('await ackMascotContextMenuLayout(generation)')
    expect(mascotMenuWindowSource).toContain('menuEntering.value = true')
    expect(nativeAck).toContain('state.can_ack_layout(generation)')
    expect(nativeAck).toContain('menu.show()')
    expect(nativeAck).toContain('menu.set_ignore_cursor_events(false)')
    expect(nativeAck).toContain('menu.set_focus()')
    expect(nativeAck).toContain('state.mark_visible(generation)')
    expect(rustSource).not.toContain('fn set_mascot_cursor_passthrough')
    expect(rustSource).toContain('fn rollback_mascot_context_menu_generation')
    expect(rustSource).toContain('fn expire_pending_show')
    expect(rustSource).toContain('A WebView2 hosted by a never-visible HWND')
    expect(rustSource).toContain('window.set_ignore_cursor_events(true)')
    expect(rustSource).toContain('show_window_without_activation(&window)')
  })

  it('remounts and refocuses the safe first action on every opening', () => {
    expect(mascotMenuWindowSource).toContain('menuGeneration.value = generation')
    expect(mascotMenuWindowSource).toContain('A generation key remounts the menu')
    expect(contextMenuSource).toContain("querySelector<HTMLButtonElement>('button')?.focus()")
    expect(contextMenuSource).toContain('watch(() => props.entering')
    expect(contextMenuSource).toContain("'is-entering': entering")
    expect(appStyles).toMatch(/\.mascot-context-menu\s*\{[\s\S]*?opacity: 0;[\s\S]*?animation: none;/)
    expect(appStyles).toMatch(/\.mascot-context-menu\.is-entering\s*\{[\s\S]*?animation:/)
  })

  it('keeps hide and exit as atomic native actions from the detached menu', () => {
    const hideAction = sourceBetween(contextMenuSource, 'function handleHide()', 'function handleExit()')
    const exitAction = sourceBetween(contextMenuSource, 'function handleExit()', 'function handleKeydown')
    const menuActions = sourceBetween(mascotMenuWindowSource, 'function handleHide()', 'function handleKeydown')
    const nativeHide = sourceBetween(rustSource, 'fn hide_main_window', 'fn show_main_window')

    // Closing the menu in a separate IPC first can suspend its WebView before
    // the hide/exit IPC is dispatched. Each action must reach its all-in-one
    // native command directly.
    expect(hideAction).toContain("emit('hide')")
    expect(hideAction).not.toContain("emit('close')")
    expect(exitAction).toContain("emit('exit')")
    expect(exitAction).not.toContain("emit('close')")
    expect(menuActions).toContain('void hideAssistant()')
    expect(menuActions).toContain('void exitAssistant()')
    expect(mascotMenuWindowSource).not.toContain('@close="closeMenu"')
    expect(nativeHide).toMatch(
      /hide_mascot_context_menu_window_with_restore\(&app, false\)[\s\S]*?hide_mascot_system_notification_native_window\(&app\)[\s\S]*?hide_transparent_window_safely\(&window\)[\s\S]*?hide_panel_and_notify\(&app\)/,
    )

    const notificationRestore = sourceBetween(
      appSource,
      'removeContextMenuVisibilityListener = await listen<MascotMenuVisibilityPayload>',
      'const nativeNotificationReady = await isMascotSystemNotificationReady()',
    )
    expect(windowServiceSource).toMatch(
      /interface MascotMenuVisibilityPayload\s*\{[\s\S]*?visible: boolean[\s\S]*?restoreNotification: boolean[\s\S]*?\}/,
    )
    expect(notificationRestore).toContain('const { visible, restoreNotification } = event.payload')
    expect(notificationRestore).toContain('if (!visible && !restoreNotification)')
    expect(notificationRestore).toContain('userHiddenSystemNotificationKey = systemNotificationMessageKey')
    expect(notificationRestore).toContain('notificationDelivery.sync(null)')
    expect(notificationRestore).toContain('if (wasVisible && !visible && restoreNotification)')
    expect(appSource).toContain('systemNotificationMessageKey === userHiddenSystemNotificationKey')
    expect(appSource).toContain("reason: suppressedByUserHide ? 'user-hide' : 'no-presentation'")
    expect(appSource).toContain("const authPresentationWasHidden = userHiddenSystemNotificationKey === 'auth'")
    expect(nativeHide).toContain('interaction.assistant_hide.completed')
  })

  it('keeps enough transparent native gutter for above and below shadows', () => {
    expect(mascotMenuWindowSource).toContain(":y=\"placement === 'above' ? 8 : 14\"")
    expect(contextMenuSource).toContain('width: `calc(100% - ${x * 2}px)`')
    expect(contextMenuSource).not.toContain('width: number')
    expect(rustSource).toContain('const MASCOT_CONTEXT_MENU_WIDTH: f64 = 216.0;')
    expect(rustSource).toContain('const MASCOT_CONTEXT_MENU_HEIGHT: f64 = 76.0;')
    expect(rustSource).toContain('const MASCOT_CONTEXT_MENU_ABOVE_VISIBLE_BOTTOM: f64 = 55.0;')
    expect(rustSource).toContain('const MASCOT_CONTEXT_MENU_BELOW_VISIBLE_TOP: f64 = 9.0;')
    expect(appStyles).toContain('0 4px 12px rgba(31, 42, 68, 0.1)')
  })

  it('closes the detached menu from native focus loss and app lifecycle paths', () => {
    expect(rustSource).toMatch(
      /WindowEvent::Focused\(false\)[\s\S]*?hide_context_menu_after_focus_moves_outside_app/
    )
    expect(rustSource).toMatch(
      /fn hide_context_menu_after_focus_moves_outside_app[\s\S]*?window\.is_focused\(\)[\s\S]*?hide_mascot_context_menu_window/
    )
    expect(mascotMenuWindowSource).toContain("if (event.key === 'Escape') closeMenu()")
    expect(windowServiceSource).toContain("invoke('hide_mascot_context_menu')")

    expect(rustSource).toMatch(
      /fn hide_main_window\b[\s\S]{0,900}?hide_mascot_context_menu_window_with_restore\(&app, false\)/,
    )
    for (const command of [
      'show_main_window',
      'show_notification_window',
      'peek_mascot_window',
      'reveal_mascot_window',
      'start_mascot_drag',
      'toggle_panel_window',
      'show_panel_window'
    ]) {
      expect(rustSource).toMatch(
        new RegExp(`fn ${command}\\b[\\s\\S]{0,900}?hide_mascot_context_menu_window\\(&app\\)`)
      )
    }
  })

  it('keeps all persistent HWNDs alive when Alt+F4 requests a close', () => {
    const tauriConfig = JSON.parse(tauriConfigSource) as {
      app: { windows: Array<{ label: string; closable?: boolean }> }
    }

    for (const label of ['mascot', 'panel', 'mascot-menu', 'mascot-notification']) {
      expect(tauriConfig.app.windows.find((window) => window.label === label)?.closable).toBe(false)
    }
    expect(rustSource.match(/tauri::WindowEvent::CloseRequested/g)).toHaveLength(4)
    expect(rustSource.match(/api\.prevent_close\(\)/g)).toHaveLength(4)
    expect(rustSource).toContain('hide_mascot_context_menu_window_with_restore(&close_app, false)')
    expect(rustSource).toContain('hide_panel_and_notify(&close_app)')
  })
})
