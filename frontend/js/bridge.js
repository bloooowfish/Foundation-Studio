(() => {
  'use strict';

  const core = window.__TAURI__?.core;
  const events = window.__TAURI__?.event;

  if (!core || !events) {
    console.error('Tauri global API is unavailable. Ensure withGlobalTauri is enabled.');
  }

  function invoke(cmd, args = {}) {
    return core.invoke(cmd, args);
  }

  function listen(eventName, handler) {
    return events.listen(eventName, handler);
  }

  async function apiRequest(path, options = {}) {
    const method = (options.method || 'GET').toUpperCase();
    let body = options.body;
    if (body !== undefined && body !== null && typeof body !== 'string') {
      body = JSON.stringify(body);
    }

    const resp = await invoke('housing_api_request', {
      method,
      path,
      body: body ?? null,
    });

    let data = null;
    if (resp.body) {
      try {
        data = JSON.parse(resp.body);
      } catch {
        data = resp.body;
      }
    }

    if (!resp.ok) {
      const errMsg =
        data && typeof data === 'object' && data.error
          ? data.error
          : resp.body || `HTTP ${resp.status}`;
      throw new Error(errMsg);
    }
    return data;
  }

  async function subscribeSse(onStatus, onEvent) {
    const unlistenStatus = await listen('housing-sse-status', (ev) => {
      onStatus(ev.payload);
    });
    const unlistenEvent = await listen('housing-sse-event', (ev) => {
      onEvent(ev.payload);
    });
    invoke('housing_reconnect_sse').catch(() => {});
    return () => {
      unlistenStatus();
      unlistenEvent();
    };
  }

  async function onDragDrop(onDrop, onEnter, onLeave) {
    const unlisteners = [];
    if (onDrop) {
      unlisteners.push(await listen('tauri://drag-drop', (ev) => onDrop(ev.payload)));
    }
    if (onEnter) {
      unlisteners.push(await listen('tauri://drag-enter', (ev) => onEnter(ev.payload)));
    }
    if (onLeave) {
      unlisteners.push(await listen('tauri://drag-leave', (ev) => onLeave(ev.payload)));
    }
    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }

  function parseProxyResponse(resp) {
    let data = null;
    if (resp.body) {
      try {
        data = JSON.parse(resp.body);
      } catch {
        data = resp.body;
      }
    }
    if (!resp.ok) {
      const errMsg =
        data && typeof data === 'object' && data.error
          ? data.error
          : resp.body || `HTTP ${resp.status}`;
      throw new Error(errMsg);
    }
    return data;
  }

  window.FoundationBridge = {
    invoke,
    listen,
    apiRequest,
    subscribeSse,
    onDragDrop,
    getSettings: () => invoke('get_settings'),
    getSystemLocale: () => invoke('get_system_locale_cmd'),
    setUiLocale: (locale) => invoke('set_ui_locale_cmd', { locale }),
    saveSettings: (settings) => invoke('save_settings_cmd', { settings }),
    readGameVersion: (gamePath) => invoke('read_game_version_cmd', { gamePath }),
    resolveReMakePlacePath: (configuredPath) =>
      invoke('resolve_remakeplace_path_cmd', { configuredPath }),
    checkServer: (serverUrl) => invoke('check_kawari_server', { serverUrl }),
    launchGame: (settings, manualVersion) =>
      invoke('launch_game', { settings, manualVersion: manualVersion ?? null }),
    getSimulatorUrl: () => invoke('housing_get_simulator_url'),
    setSimulatorUrl: (url) => invoke('housing_set_simulator_url', { url }),
    reconnectSse: () => invoke('housing_reconnect_sse'),
    scanLocalPresets: () => invoke('scan_local_presets'),
    addWatchedPresetDir: (dir) => invoke('add_watched_preset_dir', { dir }),
    removeWatchedPresetDir: (dir) => invoke('remove_watched_preset_dir', { dir }),
    inspectPresetFile: (filePath) => invoke('inspect_preset_file', { filePath }),
    previewLocalPresetFile: async (filePath, scope, landIdent) => {
      const resp = await invoke('housing_preview_local_preset_file', {
        filePath,
        scope,
        landIdent,
      });
      return parseProxyResponse(resp);
    },
    applyLocalPresetFile: async ({
      landIdent,
      filePath,
      scope,
      reloadMode,
      preservePos,
      forceEnter = false,
      label = null,
    }) => {
      const resp = await invoke('housing_apply_local_preset_file', {
        landIdent,
        filePath,
        scope,
        reloadMode,
        preservePos,
        forceEnter,
        label,
      });
      return parseProxyResponse(resp);
    },
    exportPresetToFile: (landIdent, scope, targetPath) =>
      invoke('export_preset_to_file', { landIdent, scope, targetPath }),
    revealInExplorer: (path) => invoke('reveal_in_explorer_cmd', { path }),
    getRmpStatus: () => invoke('get_rmp_status'),
    startRmpSync: (remakeplacePath) => invoke('start_rmp_sync', { remakeplacePath }),
    stopRmpSync: (killShadow = true) => invoke('stop_rmp_sync', { killShadow }),
    getServerStatus: (profile = null) => invoke('get_server_status', { profile }),
    startFoundationServer: (foundationRoot, profile) =>
      invoke('start_foundation_server', { foundationRoot, profile }),
    stopFoundationServer: () => invoke('stop_foundation_server'),
    getServerLogs: () => invoke('get_server_logs'),
    clearServerLogs: () => invoke('clear_server_logs'),
    getSetupStatus: (profile = null) => invoke('get_setup_status_cmd', { profile }),
    installRuntimeComponents: (components, useManagedDir = true, profile = null) =>
      invoke('install_runtime_components_cmd', {
        components,
        useManagedDir,
        profile,
      }),
    generateFoundationConfig: (foundationRoot = null, gamePath = null, overwrite = false) =>
      invoke('generate_foundation_config_cmd', {
        foundationRoot,
        gamePath,
        overwrite,
      }),
    setAlwaysOnTop: (pinned) => invoke('set_always_on_top_cmd', { pinned }),
    updateGlobalShortcut: (shortcut) => invoke('update_global_shortcut_cmd', { shortcut }),
    hideToTray: () => invoke('hide_to_tray_cmd'),
    openExternalUrl: (url) => invoke('open_external_url_cmd', { url }),
    pickFile: (title, defaultPath, filterName, extensions) =>
      invoke('pick_file_dialog', {
        title,
        defaultPath: defaultPath ?? null,
        filterName: filterName ?? null,
        extensions: extensions ?? null,
      }),
    pickFolder: (title, defaultPath) =>
      invoke('pick_folder_dialog', { title, defaultPath: defaultPath ?? null }),
    savePresetDialog: (title, defaultDir, defaultName) =>
      invoke('save_preset_dialog', {
        title,
        defaultDir: defaultDir ?? null,
        defaultName,
      }),
  };
})();
