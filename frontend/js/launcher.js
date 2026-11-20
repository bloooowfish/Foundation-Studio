(() => {
  'use strict';

  const Bridge = window.FoundationBridge;
  const Studio = window.FoundationStudioApp;
  const I18n = window.FoundationI18n;
  const $ = (id) => document.getElementById(id);

  let alwaysOnTop = false;
  let currentShortcut = 'Ctrl+Shift+H';
  let lastEndpoints = null;
  let rmpStatus = { phase: 'idle', running: false, relay: {} };
  let serverStatus = { mode: 'stopped', preflightIssues: [] };
  let serverLogLines = [];

  function formatNowTime() {
    return new Date().toLocaleTimeString(I18n.bcp47(), { hour12: false });
  }

  function appendLauncherLog(message) {
    const box = $('launcherStatusLog');
    if (!box) return;
    const line = `[${formatNowTime()}] ${message}`;
    box.textContent = box.textContent ? `${box.textContent}\n${line}` : line;
    box.scrollTop = box.scrollHeight;
  }

  function renderPinState(pinned) {
    alwaysOnTop = !!pinned;
    const topBtn = $('btnPinAlwaysOnTop');
    if (topBtn) {
      topBtn.classList.toggle('active', alwaysOnTop);
      topBtn.setAttribute('aria-pressed', String(alwaysOnTop));
      topBtn.textContent = I18n.t(alwaysOnTop ? 'topbar.pin_pinned' : 'topbar.pin_unpinned');
      topBtn.title = I18n.t('topbar.pin_title', { shortcut: currentShortcut });
    }
    const chk = $('prefAlwaysOnTopCheck');
    if (chk) chk.checked = alwaysOnTop;
  }

  function renderEndpoints(ep) {
    lastEndpoints = ep || null;
    const box = $('launcherEndpointsBox');
    if (!box) return;
    if (!ep) {
      box.textContent = I18n.t('launcher.endpoints_empty');
      return;
    }
    box.textContent = [
      `login: ${ep.loginServer}`,
      `lobby: ${ep.lobbyHost}:${ep.lobbyPort}`,
      `frontier: ${ep.frontierHostArgument}`,
      `simulator: ${ep.simulatorServer}`,
    ].join('\n');
  }

  async function buildMergedSettingsFromForm() {
    const current = await Bridge.getSettings();
    return {
      ...current,
      serverUrl: $('launcherServerUrl').value.trim() || 'http://127.0.0.1:21062',
      gamePath: $('launcherGamePath').value.trim(),
      username: $('launcherUsername').value.trim(),
      password: $('launcherPassword').value,
      language: Number($('launcherLanguage').value) || 0,
      expansionLevel: Number($('launcherExpansion').value) ?? 5,
      extraArguments: $('launcherExtraArgs').value,
      includeFrontierHost: !!$('launcherFrontierCheck').checked,
      openDashboardOnLaunch: !!$('launcherOpenDashboardCheck').checked,
      closeOnLaunch: !!$('launcherCloseOnLaunchCheck').checked,
      remakeplacePath: $('rmpPathInput').value.trim(),
      foundationRoot: $('srvFoundationRoot').value.trim(),
      globalShortcut: $('prefShortcutInput').value.trim() || 'Ctrl+Shift+H',
      alwaysOnTop: !!$('prefAlwaysOnTopCheck').checked,
      minimizeToTrayOnClose: !!$('prefMinimizeToTrayCheck').checked,
      uiLocale:
        $('prefUiLocaleSelect')?.value ||
        $('topbarLocaleSelect')?.value ||
        I18n.preference(),
    };
  }

  async function persistFormSettings({ quiet = true } = {}) {
    const merged = await buildMergedSettingsFromForm();
    const saved = await Bridge.saveSettings(merged);
    Studio?.syncWatchedSettings(saved.watchedPresetDirs, saved.autoApplyWatchedPresets);
    if (!quiet) {
      const msg = I18n.t('launcher.settings_saved');
      Studio?.toast(msg);
      appendLauncherLog(msg);
    }
    return saved;
  }

  async function refreshGameVersion({ quiet = true } = {}) {
    const gamePath = $('launcherGamePath').value.trim();
    if (!gamePath) return;
    try {
      const ver = await Bridge.readGameVersion(gamePath);
      $('launcherVersion').value = ver;
    } catch {
      if (!quiet) {
        appendLauncherLog(I18n.t('launcher.version_missing'));
      }
    }
  }

  function renderRmpStatus(st) {
    rmpStatus = st || { phase: 'idle', running: false, relay: {} };
    const relay = rmpStatus.relay || {};
    const phase = rmpStatus.phase || 'idle';
    const attached = !!relay.attached;

    const pillDot = $('rmpStatusDot');
    const pillText = $('rmpStatusText');
    const badge = $('rmpBadge');
    const toggleBtn = $('btnToggleRmpSync');

    if (phase === 'preparing') {
      pillDot.className = 'dot dot-warn';
      pillText.textContent = I18n.t('topbar.rmp_preparing');
      badge.className = 'badge badge-warn';
      badge.textContent = I18n.t('rmp.badge_preparing');
      toggleBtn.textContent = I18n.t('rmp.btn_preparing');
      toggleBtn.disabled = true;
    } else if (rmpStatus.running) {
      toggleBtn.disabled = false;
      toggleBtn.textContent = I18n.t('rmp.btn_stop');
      toggleBtn.className = 'btn btn-danger';
      if (attached) {
        pillDot.className = 'dot dot-ok';
        pillText.textContent = I18n.t('topbar.rmp_attached', { count: relay.actor_count ?? 0 });
        badge.className = 'badge badge-success';
        badge.textContent = I18n.t('rmp.badge_running');
      } else {
        pillDot.className = 'dot dot-warn';
        pillText.textContent = I18n.t('topbar.rmp_waiting');
        badge.className = 'badge badge-warn';
        badge.textContent = I18n.t('rmp.badge_waiting');
      }
    } else {
      toggleBtn.disabled = false;
      toggleBtn.textContent = I18n.t('rmp.btn_start');
      toggleBtn.className = 'btn btn-primary';
      pillDot.className = phase === 'error' ? 'dot dot-err' : 'dot dot-off';
      pillText.textContent = I18n.t(phase === 'error' ? 'topbar.rmp_error' : 'topbar.rmp_idle');
      badge.className = phase === 'error' ? 'badge badge-danger' : 'badge badge-muted';
      badge.textContent = I18n.t(phase === 'error' ? 'rmp.badge_error' : 'rmp.badge_idle');
    }

    $('rmpTeleHooks').textContent = `${phase} · ${relay.hooks_registered ?? 0}/${relay.hooks_total ?? 0}`;
    if (rmpStatus.running && relay.house_size) {
      $('rmpTeleScene').textContent = I18n.t('rmp.scene_fmt', {
        size: relay.house_size,
        scope: I18n.t(relay.is_outside ? 'rmp.scope_outdoor' : 'rmp.scope_indoor'),
        count: relay.actor_count ?? 0,
      });
    } else if (rmpStatus.running) {
      $('rmpTeleScene').textContent = rmpStatus.shadowPid ? `PID ${rmpStatus.shadowPid}` : '-';
    } else {
      $('rmpTeleScene').textContent = '-';
    }
    $('rmpTeleTarget').textContent = relay.target_land_ident
      ? I18n.t('rmp.tele_target_val', {
          ident: relay.target_land_ident,
          count: relay.bound_actors ?? 0,
        })
      : '-';
    $('rmpTeleCounters').textContent =
      `${relay.outbound_moves ?? 0} / ${relay.outbound_commits ?? 0} / ${relay.inbound_cmds ?? 0}`;

    const errText = relay.last_error || (phase === 'error' ? rmpStatus.message : '') || '';
    const errEl = $('rmpLastError');
    if (errText) {
      errEl.classList.remove('hidden');
      errEl.textContent = errText;
    } else {
      errEl.classList.add('hidden');
      errEl.textContent = '';
    }
  }

  function renderServerStatus(st) {
    serverStatus = st || { mode: 'stopped', preflightIssues: [] };
    const mode = serverStatus.mode || 'stopped';
    const dot = $('serverStatusDot');
    const pillText = $('serverStatusText');
    const badge = $('srvStatusBadge');
    const navCount = $('navCountLauncher');
    const startBtn = $('btnServerStart');
    const stopBtn = $('btnServerStop');

    if (mode === 'managed') {
      dot.className = 'dot dot-ok';
      pillText.textContent = I18n.t('topbar.server_managed', {
        patch: serverStatus.protocolPatch || '7.58',
      });
      badge.className = 'badge badge-success';
      badge.textContent = I18n.t('server.badge_managed', { pid: serverStatus.pid ?? '-' });
      navCount.textContent = 'ON';
      startBtn.disabled = true;
      stopBtn.disabled = false;
    } else if (mode === 'external') {
      dot.className = 'dot dot-ok';
      pillText.textContent = I18n.t('topbar.server_external', {
        patch: serverStatus.protocolPatch || '7.58',
      });
      badge.className = 'badge badge-accent';
      badge.textContent = I18n.t('server.badge_external');
      navCount.textContent = 'EXT';
      startBtn.disabled = true;
      stopBtn.disabled = true;
    } else {
      dot.className = 'dot dot-off';
      pillText.textContent = I18n.t('topbar.server_stopped');
      badge.className = 'badge badge-muted';
      badge.textContent = I18n.t('server.badge_stopped');
      navCount.textContent = 'OFF';
      startBtn.disabled = false;
      stopBtn.disabled = true;
    }

    const pf = $('srvPreflightBox');
    const issues = Array.isArray(serverStatus.preflightIssues) ? serverStatus.preflightIssues : [];
    if (mode === 'stopped' && issues.length > 0) {
      pf.classList.remove('hidden');
      const formatted = issues.map((item) => I18n.formatPreflightIssue(item)).join(' / ');
      pf.textContent = I18n.t('server.preflight_prefix', { issues: formatted });
    } else {
      pf.classList.add('hidden');
      pf.textContent = '';
    }
  }

  async function refreshServerStatus() {
    try {
      const profile = $('srvProfileSelect')?.value || 'debug';
      const st = await Bridge.getServerStatus(profile);
      renderServerStatus(st);
      return st;
    } catch {
      return null;
    }
  }

  function renderServerConsole() {
    const el = $('srvConsoleLogs');
    if (!el) return;
    if (!serverLogLines.length) {
      el.textContent = I18n.t('server.console_empty');
      return;
    }
    el.textContent = serverLogLines.join('\n');
    el.scrollTop = el.scrollHeight;
  }

  function appendServerLogEntry(entry) {
    if (!entry) return;
    const t = new Date(entry.tsMs || Date.now()).toLocaleTimeString(I18n.bcp47(), { hour12: false });
    serverLogLines.push(`[${t}] [${entry.stream}] ${entry.line}`);
    if (serverLogLines.length > 400) {
      serverLogLines = serverLogLines.slice(-400);
    }
    renderServerConsole();
  }

  async function checkKawariConnection({ quiet = false } = {}) {
    const rawUrl = $('launcherServerUrl').value.trim();
    const res = await Bridge.checkServer(rawUrl);
    $('launcherServerUrl').value = res.normalizedUrl;
    renderEndpoints(res.endpoints);
    await persistFormSettings({ quiet: true });
    if (!quiet) {
      const msg = I18n.t('launcher.check_log', {
        url: res.normalizedUrl,
        sim: res.endpoints.simulatorServer,
      });
      appendLauncherLog(msg);
      Studio?.pushExternalLog('launcher_check', 'launcher', msg);
      Studio?.toast(I18n.t('launcher.check_ok'));
    }
    await refreshServerStatus();
    await Studio?.refreshAll(null, { forceSync: false, silentError: true });
    return res;
  }

  function bindEvents() {
    $('serverStatusPill')?.addEventListener('click', () => Studio?.switchTab('launcher'));
    $('rmpStatusPill')?.addEventListener('click', () => Studio?.switchTab('launcher'));

    const handleLocaleChange = async (ev) => {
      try {
        await I18n.setLocale(ev.target.value);
      } catch (err) {
        Studio?.toast(I18n.t('launcher.settings_save_fail', { error: err.message }), true);
      }
    };
    $('topbarLocaleSelect')?.addEventListener('change', handleLocaleChange);
    $('prefUiLocaleSelect')?.addEventListener('change', handleLocaleChange);

    const togglePin = async (targetPinned) => {
      try {
        const applied = await Bridge.setAlwaysOnTop(targetPinned);
        renderPinState(applied);
      } catch (err) {
        Studio?.toast(I18n.t('topbar.pin_fail', { error: err.message }), true);
      }
    };

    $('btnPinAlwaysOnTop')?.addEventListener('click', () => togglePin(!alwaysOnTop));
    $('prefAlwaysOnTopCheck')?.addEventListener('change', (ev) => togglePin(!!ev.target.checked));
    $('prefMinimizeToTrayCheck')?.addEventListener('change', () => persistFormSettings({ quiet: true }));

    $('btnApplyShortcut')?.addEventListener('click', async () => {
      const sc = $('prefShortcutInput').value.trim();
      try {
        const applied = await Bridge.updateGlobalShortcut(sc);
        $('prefShortcutInput').value = applied;
        currentShortcut = applied || 'Ctrl+Shift+H';
        renderPinState(alwaysOnTop);
        const msg = I18n.t('desktop.shortcut_applied', { shortcut: applied });
        Studio?.toast(msg);
        appendLauncherLog(msg);
      } catch (err) {
        Studio?.toast(I18n.t('desktop.shortcut_fail', { error: err.message }), true);
      }
    });

    $('launcherServerUrl')?.addEventListener('input', () => renderEndpoints(null));
    $('launcherGamePath')?.addEventListener('blur', () => refreshGameVersion({ quiet: true }));

    $('btnBrowseGamePath')?.addEventListener('click', async () => {
      const picked = await Bridge.pickFile(
        I18n.t('dialog.pick_game'),
        $('launcherGamePath').value.trim() || null,
        I18n.t('dialog.filter_ffxiv'),
        ['exe']
      );
      if (!picked) return;
      $('launcherGamePath').value = picked;
      await refreshGameVersion({ quiet: false });
      await persistFormSettings({ quiet: true });
    });

    $('btnRefreshLauncher')?.addEventListener('click', async () => {
      await refreshGameVersion({ quiet: false });
      try {
        await checkKawariConnection({ quiet: false });
      } catch (err) {
        appendLauncherLog(I18n.t('launcher.error_prefix', { msg: err.message }));
        Studio?.toast(I18n.t('launcher.check_fail', { error: err.message }), true);
      }
    });

    $('btnSaveLauncherSettings')?.addEventListener('click', async () => {
      try {
        await persistFormSettings({ quiet: false });
      } catch (err) {
        Studio?.toast(I18n.t('launcher.settings_save_fail', { error: err.message }), true);
      }
    });

    $('btnCheckKawari')?.addEventListener('click', async () => {
      const btn = $('btnCheckKawari');
      btn.disabled = true;
      try {
        await checkKawariConnection({ quiet: false });
      } catch (err) {
        appendLauncherLog(I18n.t('launcher.error_prefix', { msg: err.message }));
        Studio?.toast(I18n.t('launcher.check_fail', { error: err.message }), true);
      } finally {
        btn.disabled = false;
      }
    });

    $('btnLaunchGame')?.addEventListener('click', async () => {
      const btn = $('btnLaunchGame');
      btn.disabled = true;
      try {
        const merged = await buildMergedSettingsFromForm();
        const manualVersion = $('launcherVersion').value.trim();
        appendLauncherLog(I18n.t('launcher.launching', { url: merged.serverUrl }));
        const res = await Bridge.launchGame(merged, manualVersion || null);
        renderEndpoints(res.endpoints);
        for (const w of res.warnings || []) {
          appendLauncherLog(I18n.t('launcher.warn_prefix', { msg: w }));
        }
        appendLauncherLog(
          I18n.t('launcher.launch_log_pid', { pid: res.pid, path: res.executablePath })
        );
        appendLauncherLog(I18n.t('launcher.launch_log_args', { args: res.maskedArguments }));
        const toastMsg = I18n.t('launcher.launch_done_toast', { pid: res.pid });
        Studio?.pushExternalLog('launcher_launch', 'launcher', toastMsg);
        Studio?.toast(toastMsg);
        if (merged.openDashboardOnLaunch && !merged.closeOnLaunch) {
          Studio?.switchTab('estates');
        }
      } catch (err) {
        appendLauncherLog(I18n.t('launcher.error_prefix', { msg: err.message }));
        Studio?.toast(I18n.t('launcher.launch_fail_toast', { error: err.message }), true);
      } finally {
        btn.disabled = false;
      }
    });

    $('btnBrowseRmpPath')?.addEventListener('click', async () => {
      const picked = await Bridge.pickFile(
        I18n.t('dialog.pick_rmp'),
        $('rmpPathInput').value.trim() || null,
        I18n.t('dialog.filter_rmp'),
        ['exe']
      );
      if (!picked) return;
      try {
        const resolved = await Bridge.resolveReMakePlacePath(picked);
        $('rmpPathInput').value = resolved || picked;
        await persistFormSettings({ quiet: true });
        appendLauncherLog(I18n.t('rmp.path_saved', { path: $('rmpPathInput').value }));
      } catch (err) {
        Studio?.toast(I18n.t('rmp.path_fail', { error: err.message }), true);
      }
    });

    $('btnToggleRmpSync')?.addEventListener('click', async () => {
      if (rmpStatus.phase === 'preparing') return;
      if (rmpStatus.running) {
        const st = await Bridge.stopRmpSync(true);
        renderRmpStatus(st);
        const stopMsg = I18n.t('rmp.stopped_log');
        appendLauncherLog(stopMsg);
        Studio?.pushExternalLog('rmp_stopped', 'rmp', stopMsg);
        return;
      }
      const rmpPath = $('rmpPathInput').value.trim();
      if (!rmpPath) {
        Studio?.toast(I18n.t('rmp.missing_path'), true);
        return;
      }
      try {
        await persistFormSettings({ quiet: true });
        appendLauncherLog(I18n.t('rmp.starting_log', { path: rmpPath }));
        const st = await Bridge.startRmpSync(rmpPath);
        renderRmpStatus(st);
        Studio?.pushExternalLog(
          'rmp_started',
          'rmp',
          I18n.t('rmp.started_log', { pid: st.shadowPid })
        );
      } catch (err) {
        appendLauncherLog(`ERROR (RMP): ${err.message}`);
        Studio?.toast(I18n.t('rmp.start_fail', { error: err.message }), true);
      }
    });

    $('btnBrowseFoundationRoot')?.addEventListener('click', async () => {
      const picked = await Bridge.pickFolder(
        I18n.t('dialog.pick_foundation_root'),
        $('srvFoundationRoot').value.trim() || null
      );
      if (!picked) return;
      $('srvFoundationRoot').value = picked;
      await persistFormSettings({ quiet: true });
      await refreshServerStatus();
    });

    $('srvProfileSelect')?.addEventListener('change', () => refreshServerStatus());
    $('btnServerRefresh')?.addEventListener('click', async () => {
      await persistFormSettings({ quiet: true });
      await refreshServerStatus();
    });

    $('btnServerStart')?.addEventListener('click', async () => {
      const btn = $('btnServerStart');
      btn.disabled = true;
      try {
        await persistFormSettings({ quiet: true });
        const root = $('srvFoundationRoot').value.trim();
        const profile = $('srvProfileSelect').value;
        appendLauncherLog(I18n.t('server.starting', { profile }));
        const st = await Bridge.startFoundationServer(root, profile);
        renderServerStatus(st);
        $('srvConsoleDetails').open = true;
        Studio?.pushExternalLog(
          'server_started',
          'server',
          I18n.t('server.started_log', { pid: st.pid ?? '-' })
        );
        Studio?.toast(I18n.t('server.started_toast'));
      } catch (err) {
        appendLauncherLog(`ERROR (Server): ${err.message}`);
        Studio?.toast(I18n.t('server.start_fail', { error: err.message }), true);
        await refreshServerStatus();
      }
    });

    $('btnServerStop')?.addEventListener('click', async () => {
      try {
        await Bridge.stopFoundationServer();
        const stopMsg = I18n.t('server.stopped_toast');
        appendLauncherLog(stopMsg);
        Studio?.pushExternalLog('server_stopped', 'server', stopMsg);
        await refreshServerStatus();
      } catch (err) {
        Studio?.toast(I18n.t('server.stop_fail', { error: err.message }), true);
      }
    });

    $('btnClearServerLogs')?.addEventListener('click', async () => {
      await Bridge.clearServerLogs();
      serverLogLines = [];
      renderServerConsole();
    });
  }

  function subscribeBackendEvents() {
    Bridge.listen('rmp-sync-status', (ev) => {
      renderRmpStatus(ev.payload);
    });
    Bridge.listen('rmp-sync-log', (ev) => {
      const p = ev.payload;
      if (!p) return;
      appendLauncherLog(`[RMP] ${p.message}`);
      Studio?.pushExternalLog('rmp_sync', 'rmp', p.message);
    });
    Bridge.listen('foundation-server-changed', () => {
      refreshServerStatus();
    });
    Bridge.listen('foundation-server-log', (ev) => {
      appendServerLogEntry(ev.payload);
    });
    Bridge.listen('always-on-top-changed', (ev) => {
      renderPinState(!!ev.payload);
    });
    Bridge.listen('app-toast', (ev) => {
      const p = ev.payload;
      if (!p) return;
      if (typeof p === 'object' && p.key) {
        Studio?.toast(I18n.t(p.key, p.params || {}));
      } else {
        Studio?.toast(String(p));
      }
    });
  }

  async function init() {
    const settings = await Bridge.getSettings();
    await I18n.init(settings?.uiLocale || 'auto');

    I18n.onChange(() => {
      renderPinState(alwaysOnTop);
      renderEndpoints(lastEndpoints);
      renderRmpStatus(rmpStatus);
      renderServerStatus(serverStatus);
      renderServerConsole();
    });

    bindEvents();
    subscribeBackendEvents();

    currentShortcut = settings.globalShortcut || 'Ctrl+Shift+H';
    $('launcherServerUrl').value = settings.serverUrl || 'http://127.0.0.1:21062';
    $('launcherGamePath').value = settings.gamePath || '';
    $('launcherUsername').value = settings.username || '';
    $('launcherPassword').value = settings.password || '';
    $('launcherLanguage').value = String(settings.language ?? 0);
    $('launcherExpansion').value = String(settings.expansionLevel ?? 5);
    $('launcherExtraArgs').value = settings.extraArguments || '';
    $('launcherFrontierCheck').checked = settings.includeFrontierHost !== false;
    $('launcherOpenDashboardCheck').checked = settings.openDashboardOnLaunch !== false;
    $('launcherCloseOnLaunchCheck').checked = !!settings.closeOnLaunch;
    $('rmpPathInput').value = settings.remakeplacePath || '';
    $('srvFoundationRoot').value = settings.foundationRoot || '';
    $('prefShortcutInput').value = currentShortcut;
    $('prefMinimizeToTrayCheck').checked = settings.minimizeToTrayOnClose !== false;
    renderPinState(!!settings.alwaysOnTop);

    Studio?.syncWatchedSettings(settings.watchedPresetDirs, settings.autoApplyWatchedPresets);
    await Studio?.refreshLocalPresets();

    await refreshGameVersion({ quiet: true });

    const [rmp, srv, existingLogs] = await Promise.all([
      Bridge.getRmpStatus().catch(() => null),
      refreshServerStatus(),
      Bridge.getServerLogs().catch(() => []),
    ]);
    if (rmp) renderRmpStatus(rmp);
    if (Array.isArray(existingLogs) && existingLogs.length) {
      existingLogs.forEach(appendServerLogEntry);
    }

    appendLauncherLog(I18n.t('launcher.ready'));
    if (srv && srv.mode === 'stopped') {
      Studio?.switchTab('launcher');
    }

    setInterval(() => {
      refreshServerStatus();
    }, 5000);
  }

  init().catch((err) => {
    console.error('Launcher init error:', err);
  });
})();
