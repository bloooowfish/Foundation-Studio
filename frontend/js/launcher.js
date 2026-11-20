(() => {
  'use strict';

  const Bridge = window.FoundationBridge;
  const Studio = window.FoundationStudioApp;
  const I18n = window.FoundationI18n;
  const $ = (id) => document.getElementById(id);

  const ONBOARDING_DONE_KEY = 'foundation_studio_onboarding_done_v1';

  let alwaysOnTop = false;
  let currentShortcut = 'Ctrl+Shift+H';
  let lastEndpoints = null;
  let rmpStatus = { phase: 'idle', running: false, relay: {} };
  let serverStatus = { mode: 'stopped', preflightIssues: [] };
  let setupStatus = null;
  let updateInfo = null;
  let setupUiInitialized = false;
  let serverLogLines = [];
  let rmpInstallBusy = false;

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

  function isOnboardingDone() {
    try {
      return localStorage.getItem(ONBOARDING_DONE_KEY) === '1';
    } catch {
      return false;
    }
  }

  function markOnboardingDone() {
    try {
      localStorage.setItem(ONBOARDING_DONE_KEY, '1');
    } catch {
      // Ignore storage errors.
    }
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
      uiLocale: $('prefUiLocaleSelect')?.value || I18n.preference(),
      gameDataLocale: $('prefGameDataLocaleSelect')?.value || I18n.gamePreference(),
      koGameDataPath: ($('prefKoGameDataPath')?.value || '').trim(),
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

  async function reloadPathFieldsFromSettings() {
    const settings = await Bridge.getSettings();
    $('srvFoundationRoot').value = settings.foundationRoot || '';
    $('rmpPathInput').value = settings.remakeplacePath || '';
    $('onbRmpDir').value = stripExeFromPath(settings.remakeplacePath || '');
    if ($('prefKoGameDataPath')) {
      $('prefKoGameDataPath').value = settings.koGameDataPath || '';
    }
    renderRmpPathLabels();
    return settings;
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

  function setStatusBadge(el, badgeClass, text) {
    if (!el) return;
    el.className = `badge ${badgeClass}`;
    el.textContent = text;
  }

  function activeFoundationDir() {
    return (setupStatus?.activeFoundationDir || $('srvFoundationRoot')?.value || '').trim();
  }

  function activeConfigPath() {
    const root = activeFoundationDir();
    return root ? `${root.replace(/[\\/]+$/, '')}\\config.yaml` : '';
  }

  function managedRuntimeRootDir() {
    const mf = (setupStatus?.managedFoundationDir || '').trim();
    return mf ? mf.replace(/[\\/]foundation[\\/]?$/i, '') : '';
  }

  async function openPathInExplorer(path) {
    const target = String(path || '').trim();
    if (!target) return;
    try {
      await Bridge.revealInExplorer(target);
    } catch (err) {
      Studio?.toast(I18n.t('presets.reveal_fail', { error: err.message }), true);
    }
  }

  function syncOpenPathButtons() {
    const fDir = activeFoundationDir();
    const hasCfg = !!setupStatus?.foundationHasConfig && !!fDir;
    const rmpPath = ($('rmpPathInput')?.value || '').trim();
    const gamePath = ($('launcherGamePath')?.value || '').trim();
    const koGameDataPath = ($('prefKoGameDataPath')?.value || '').trim();
    const bridgeDir = (setupStatus?.activeBridgeDir || '').trim();
    const managedDir = managedRuntimeRootDir();

    if ($('btnOpenFoundationDir')) $('btnOpenFoundationDir').disabled = !fDir;
    if ($('btnOpenConfigFile')) $('btnOpenConfigFile').disabled = !hasCfg;
    if ($('btnOpenRmpDir')) $('btnOpenRmpDir').disabled = !rmpPath;
    if ($('btnOpenBridgeDir')) $('btnOpenBridgeDir').disabled = !bridgeDir;
    if ($('btnOpenManagedDir')) $('btnOpenManagedDir').disabled = !managedDir;
    if ($('btnSettingsOpenGame')) $('btnSettingsOpenGame').disabled = !gamePath;
    if ($('btnSettingsOpenFoundation')) $('btnSettingsOpenFoundation').disabled = !fDir;
    if ($('btnSettingsOpenConfig')) $('btnSettingsOpenConfig').disabled = !hasCfg;
    if ($('btnSettingsOpenRmp')) $('btnSettingsOpenRmp').disabled = !rmpPath;
    if ($('btnSettingsOpenKoGameData')) $('btnSettingsOpenKoGameData').disabled = !koGameDataPath;
  }

  function renderGameDataLocaleHint() {
    const el = $('prefGameDataLocaleHint');
    if (!el) return;
    const info = I18n.gameDataInfo ? I18n.gameDataInfo() : null;
    if (!info || !info.gameDirValid) {
      el.textContent = I18n.t('desktop.game_data_hint_no_game');
      return;
    }
    const langs = (Array.isArray(info.availableLanguages) ? info.availableLanguages.join(', ') : '') || '-';
    if (info.usingKoDataDir) {
      el.textContent = I18n.t('desktop.game_data_hint_ko_external', { langs });
    } else if (info.requestedInSqpack) {
      el.textContent = I18n.t('desktop.game_data_hint_ok', {
        effective: info.effectiveLocale,
        langs,
      });
    } else if (info.requestedLocale === 'ko' && !info.koDataDirValid) {
      el.textContent = I18n.t('desktop.game_data_hint_ko_missing', {
        effective: info.effectiveLocale,
        langs,
      });
    } else {
      el.textContent = I18n.t('desktop.game_data_hint_fallback', {
        requested: info.requestedLocale,
        effective: info.effectiveLocale,
        langs,
      });
    }
  }

  function renderRmpPathLabels() {
    const path = $('rmpPathInput').value.trim();
    const homePath = $('homeRmpPath');
    if (homePath) homePath.textContent = path || I18n.t('home.rmp_path_none');
    const rtPath = $('runtimeRmpPath');
    if (rtPath) rtPath.textContent = path || '-';
    syncOpenPathButtons();
  }

  // ---- ReMakePlace live sync ----

  function renderRmpStatus(st) {
    rmpStatus = st || { phase: 'idle', running: false, relay: {} };
    const relay = rmpStatus.relay || {};
    const phase = rmpStatus.phase || 'idle';
    const attached = !!relay.attached;

    const pillDot = $('rmpStatusDot');
    const pillText = $('rmpStatusText');
    const badge = $('rmpBadge');
    const toggleBtn = $('btnToggleRmpSync');
    const decorBtn = $('btnDecorRmpToggle');

    let badgeClass;
    let badgeText;
    let btnText;
    let btnClass;
    let btnDisabled = false;

    if (phase === 'preparing') {
      pillDot.className = 'dot dot-warn';
      pillText.textContent = I18n.t('topbar.rmp_preparing');
      badgeClass = 'badge-warn';
      badgeText = I18n.t('rmp.badge_preparing');
      btnText = I18n.t('rmp.btn_preparing');
      btnClass = 'btn btn-primary';
      btnDisabled = true;
    } else if (rmpStatus.running) {
      btnText = I18n.t('rmp.btn_stop');
      btnClass = 'btn btn-danger';
      if (attached) {
        pillDot.className = 'dot dot-ok';
        pillText.textContent = I18n.t('topbar.rmp_attached', { count: relay.actor_count ?? 0 });
        badgeClass = 'badge-success';
        badgeText = I18n.t('rmp.badge_running');
      } else {
        pillDot.className = 'dot dot-warn';
        pillText.textContent = I18n.t('topbar.rmp_waiting');
        badgeClass = 'badge-warn';
        badgeText = I18n.t('rmp.badge_waiting');
      }
    } else {
      btnText = I18n.t('rmp.btn_start');
      btnClass = 'btn btn-primary';
      pillDot.className = phase === 'error' ? 'dot dot-err' : 'dot dot-off';
      pillText.textContent = I18n.t(phase === 'error' ? 'topbar.rmp_error' : 'topbar.rmp_idle');
      badgeClass = phase === 'error' ? 'badge-danger' : 'badge-muted';
      badgeText = I18n.t(phase === 'error' ? 'rmp.badge_error' : 'rmp.badge_idle');
    }

    setStatusBadge(badge, badgeClass, badgeText);
    setStatusBadge($('decorRmpBadge'), badgeClass, badgeText);
    toggleBtn.textContent = btnText;
    toggleBtn.className = btnClass;
    toggleBtn.disabled = btnDisabled;
    if (decorBtn) {
      decorBtn.textContent = btnText;
      decorBtn.className = `${btnClass} btn-sm`;
      decorBtn.disabled = btnDisabled;
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

  // ---- Server ----

  function renderServerStatus(st) {
    serverStatus = st || { mode: 'stopped', preflightIssues: [] };
    const mode = serverStatus.mode || 'stopped';
    const dot = $('serverStatusDot');
    const pillText = $('serverStatusText');
    const badge = $('srvStatusBadge');
    const startBtn = $('btnServerStart');
    const stopBtn = $('btnServerStop');
    const launchBtn = $('btnLaunchGame');
    const running = mode === 'managed' || mode === 'external';

    if (mode === 'managed') {
      dot.className = 'dot dot-ok';
      pillText.textContent = I18n.t('topbar.server_managed', {
        patch: serverStatus.protocolPatch || '7.58',
      });
      setStatusBadge(badge, 'badge-success', I18n.t('server.badge_managed', { pid: serverStatus.pid ?? '-' }));
      startBtn.disabled = true;
      stopBtn.disabled = false;
    } else if (mode === 'external') {
      dot.className = 'dot dot-ok';
      pillText.textContent = I18n.t('topbar.server_external', {
        patch: serverStatus.protocolPatch || '7.58',
      });
      setStatusBadge(badge, 'badge-accent', I18n.t('server.badge_external'));
      startBtn.disabled = true;
      stopBtn.disabled = true;
    } else {
      dot.className = 'dot dot-off';
      pillText.textContent = I18n.t('topbar.server_stopped');
      setStatusBadge(badge, 'badge-muted', I18n.t('server.badge_stopped'));
      startBtn.disabled = startBtn.dataset.busy === '1';
      stopBtn.disabled = true;
    }

    if (launchBtn && launchBtn.dataset.busy !== '1') {
      launchBtn.disabled = !running;
      launchBtn.title = running ? '' : I18n.t('home.game_needs_server');
    }
    setStatusBadge(
      $('homeGameBadge'),
      running ? 'badge-success' : 'badge-muted',
      I18n.t(running ? 'home.game_ready' : 'home.game_needs_server')
    );

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
      if (st?.profile && $('srvProfileSelect') && st.profile !== $('srvProfileSelect').value) {
        $('srvProfileSelect').value = st.profile;
      }
      renderServerStatus(st);
      return st;
    } catch {
      return null;
    }
  }

  // ---- Runtime setup ----

  function requiredReady(st) {
    return !!(
      st
      && st.foundationInstalled
      && st.foundationHasOodleFeature
      && st.oodleInstalled
      && st.foundationHasConfig
    );
  }

  function renderOnboarding() {
    const st = setupStatus;
    const onb = $('homeOnboarding');
    const main = $('homeMain');
    if (!onb || !main || !st) return;

    const ready = requiredReady(st);
    const gameOk = !!st.gamePathValid;
    const showOnb = !ready || !gameOk || !isOnboardingDone();
    onb.classList.toggle('hidden', !showOnb);
    main.classList.toggle('hidden', showOnb);

    setStatusBadge(
      $('onbStateGame'),
      gameOk ? 'badge-success' : 'badge-warn',
      I18n.t(gameOk ? 'home.onb_game_ok' : 'setup.state_missing')
    );
    setStatusBadge(
      $('onbStateRuntime'),
      st.busy ? 'badge-warn' : ready ? 'badge-success' : 'badge-warn',
      I18n.t(st.busy ? 'setup.badge_busy' : ready ? 'setup.state_ok' : 'setup.state_missing')
    );
    setStatusBadge(
      $('onbStateRmp'),
      st.remakeplaceValid ? 'badge-success' : 'badge-muted',
      I18n.t(st.remakeplaceValid ? 'setup.state_ok' : 'home.onb_optional')
    );

    const installBtn = $('btnOnbInstall');
    if (installBtn) installBtn.disabled = !!st.busy || ready || !gameOk;
    const startBtn = $('btnOnbStart');
    if (startBtn) startBtn.disabled = !(ready && gameOk);
    const hint = $('onbFooterHint');
    if (hint) {
      hint.textContent = I18n.t(
        !gameOk ? 'home.onb_need_game' : !ready ? 'home.onb_start_hint' : 'home.onb_ready_hint'
      );
    }
    ['btnOnbRmpInstall', 'btnOnbRmpExisting'].forEach((id) => {
      const el = $(id);
      if (el) el.disabled = !!st.busy || rmpInstallBusy;
    });
  }

  const COMPONENT_UI = {
    foundation: { item: 'foundation', info: 'runtimeFoundationInfo', srcKey: 'setup.comp_foundation_src', nameKey: 'setup.comp_foundation' },
    oodle: { item: 'oodle', info: 'runtimeOodleInfo', srcKey: 'setup.comp_oodle_src', nameKey: 'setup.comp_oodle' },
    rmp_bridge: { item: 'rmpBridge', info: 'runtimeBridgeInfo', srcKey: 'setup.comp_bridge_src', nameKey: 'setup.comp_bridge' },
    ue4ss: { item: 'ue4ss', info: 'runtimeUe4ssInfo', srcKey: 'setup.comp_ue4ss_src', nameKey: 'setup.comp_ue4ss' },
  };

  function componentInstalled(comp) {
    const st = setupStatus || {};
    if (comp === 'foundation') return !!st.foundationInstalled;
    if (comp === 'oodle') return !!st.oodleInstalled;
    if (comp === 'rmp_bridge') return !!st.rmpBridgeInstalled;
    if (comp === 'ue4ss') return !!st.ue4ssInstalled;
    return false;
  }

  function describeItem(item, fallbackKey) {
    if (!item) return I18n.t(fallbackKey);
    if (item.source === 'dev') return I18n.t('runtime.source_dev_info');
    if (item.pinned && item.installed) return I18n.t('runtime.pinned_version', { version: item.installed });
    if (item.updateAvailable) {
      return I18n.t('runtime.update_available', { installed: item.installed || '-', latest: item.latest });
    }
    if (item.installed && item.latest) return I18n.t('runtime.up_to_date', { version: item.installed });
    if (item.error) return I18n.t('runtime.check_error');
    if (item.latest) return I18n.t('runtime.latest_version', { version: item.latest });
    return I18n.t(fallbackKey);
  }

  function renderUpdateInfo() {
    const busy = rmpInstallBusy || !!setupStatus?.busy;
    Object.entries(COMPONENT_UI).forEach(([comp, ui]) => {
      const item = updateInfo?.[ui.item];
      const infoEl = $(ui.info);
      if (infoEl) infoEl.textContent = describeItem(item, ui.srcKey);
      const btn = document.querySelector(`[data-setup-install="${comp}"]`);
      if (!btn) return;
      let mode = 'install';
      let labelKey = 'setup.btn_install_one';
      let cls = 'btn btn-secondary btn-xs';
      let locked = false;
      let title = '';
      if (componentInstalled(comp)) {
        if (item?.source === 'dev') {
          labelKey = 'runtime.btn_reinstall';
          cls = 'btn btn-ghost btn-xs';
          locked = true;
          title = I18n.t('runtime.dev_locked');
        } else if (item?.canUpdate) {
          mode = 'update';
          labelKey = 'runtime.btn_update';
          cls = 'btn btn-primary btn-xs';
        } else {
          labelKey = 'runtime.btn_reinstall';
          cls = 'btn btn-ghost btn-xs';
        }
      }
      btn.dataset.mode = mode;
      btn.className = cls;
      btn.textContent = I18n.t(labelKey);
      btn.title = title;
      btn.disabled = busy || locked;
    });

    const rInfo = $('runtimeRmpInfo');
    const r = updateInfo?.remakeplace;
    if (rInfo) rInfo.textContent = r ? describeItem(r, 'setup.comp_rmp_src') : '';
    const rmpBtn = $('btnRmpInstall');
    if (rmpBtn) {
      const installed = !!setupStatus?.remakeplaceValid;
      const canUpdate = installed && (r ? (r.canUpdate ?? r.updateAvailable) : false);
      rmpBtn.textContent = I18n.t(installed ? 'runtime.rmp_update' : 'runtime.rmp_install');
      rmpBtn.className = installed && !canUpdate ? 'btn btn-ghost btn-xs' : 'btn btn-primary btn-xs';
      rmpBtn.disabled = busy || (installed && !canUpdate);
    }
    const repairBtn = $('btnRmpRepair');
    if (repairBtn) repairBtn.disabled = busy || !setupStatus?.remakeplaceValid;
    renderNavRuntimeBadge();
  }

  function renderNavRuntimeBadge() {
    const badge = $('navBadgeRuntime');
    if (!badge) return;
    const anyUpdate = updateInfo
      && ['foundation', 'oodle', 'rmpBridge', 'remakeplace'].some((k) => {
        const it = updateInfo[k];
        return !!(it && (it.canUpdate ?? (it.updateAvailable && it.installed)));
      });
    const needsAttention = (setupStatus && !requiredReady(setupStatus)) || !!anyUpdate;
    badge.classList.toggle('hidden', !needsAttention);
  }

  function sourceLabel(source) {
    const known = ['managed', 'custom', 'dev', 'missing'];
    return I18n.t(`runtime.source_${known.includes(source) ? source : 'missing'}`);
  }

  function renderInstallLocation() {
    const st = setupStatus;
    if (!st) return;
    const managedRoot = (st.managedFoundationDir || '').replace(/[\\/]foundation[\\/]?$/i, '');
    $('setupManagedPath').textContent = managedRoot || st.managedFoundationDir || '-';
    $('setupSummaryPath').textContent = st.activeFoundationDir || '-';
    $('setupBridgePath').textContent = st.activeBridgeDir || '-';
    const fSrc = st.foundationSource || (st.foundationInstalled ? 'custom' : 'missing');
    const bSrc = st.bridgeSource || (st.rmpBridgeInstalled ? 'custom' : 'missing');
    setStatusBadge($('setupFoundationSource'), fSrc === 'managed' ? 'badge-success' : fSrc === 'dev' ? 'badge-accent' : 'badge-muted', sourceLabel(fSrc));
    setStatusBadge($('setupBridgeSource'), bSrc === 'managed' ? 'badge-success' : bSrc === 'dev' ? 'badge-accent' : 'badge-muted', sourceLabel(bSrc));
    $('setupDevNote').classList.toggle('hidden', fSrc !== 'dev' && bSrc !== 'dev');
  }

  function renderSetupStatus(st) {
    setupStatus = st || null;
    if (!setupStatus) return;

    const okFoundation = setupStatus.foundationInstalled && setupStatus.foundationHasOodleFeature;
    if (!setupStatus.foundationInstalled) {
      setStatusBadge($('setupStateFoundation'), 'badge-danger', I18n.t('setup.state_missing'));
    } else if (!setupStatus.foundationHasOodleFeature) {
      setStatusBadge($('setupStateFoundation'), 'badge-warn', I18n.t('setup.state_needs_oodle'));
    } else {
      const detail = setupStatus.foundationVersion || setupStatus.foundationLayout || 'ok';
      setStatusBadge($('setupStateFoundation'), 'badge-success', I18n.t('setup.state_ok_ver', { detail }));
    }

    setStatusBadge(
      $('setupStateOodle'),
      setupStatus.oodleInstalled ? 'badge-success' : 'badge-danger',
      I18n.t(setupStatus.oodleInstalled ? 'setup.state_ok' : 'setup.state_missing')
    );
    setStatusBadge(
      $('setupStateBridge'),
      setupStatus.rmpBridgeInstalled ? 'badge-success' : 'badge-muted',
      I18n.t(setupStatus.rmpBridgeInstalled ? 'setup.state_ok' : 'setup.state_missing')
    );
    setStatusBadge(
      $('setupStateUe4ss'),
      setupStatus.ue4ssInstalled ? 'badge-success' : 'badge-muted',
      I18n.t(setupStatus.ue4ssInstalled ? 'setup.state_ok' : 'setup.state_missing')
    );

    const localOnly = setupStatus.configLocalhostOnly !== false;
    if (!setupStatus.foundationHasConfig) {
      setStatusBadge($('setupStateConfig'), 'badge-danger', I18n.t('setup.state_missing'));
    } else if (!localOnly) {
      setStatusBadge($('setupStateConfig'), 'badge-warn', I18n.t('runtime.config_not_local_badge'));
    } else {
      setStatusBadge($('setupStateConfig'), 'badge-success', I18n.t('setup.state_ok'));
    }
    const cfgInfo = $('runtimeConfigInfo');
    if (cfgInfo) {
      cfgInfo.textContent = setupStatus.foundationHasConfig && !localOnly
        ? I18n.t('runtime.config_not_local')
        : I18n.t('setup.comp_config_src');
    }

    if (setupStatus.remakeplaceValid) {
      const ver = setupStatus.remakeplaceInstalledVersion;
      setStatusBadge(
        $('setupStateRmp'),
        'badge-success',
        ver ? I18n.t('setup.state_ok_ver', { detail: ver }) : I18n.t('setup.state_ok')
      );
    } else {
      setStatusBadge($('setupStateRmp'), 'badge-muted', I18n.t('setup.state_missing'));
    }

    const ready = okFoundation && setupStatus.oodleInstalled && setupStatus.foundationHasConfig;
    if (setupStatus.busy) {
      setStatusBadge($('setupBadgeOverall'), 'badge-warn', I18n.t('setup.badge_busy'));
    } else if (ready) {
      setStatusBadge($('setupBadgeOverall'), 'badge-success', I18n.t('setup.badge_ready'));
    } else {
      setStatusBadge($('setupBadgeOverall'), 'badge-warn', I18n.t('setup.badge_incomplete'));
    }

    if (!setupUiInitialized) {
      setupUiInitialized = true;
      const managedRadio = $('setupUseManagedDirCheck');
      const customRadio = $('setupTargetCustom');
      if (managedRadio && customRadio) {
        const activeLow = (setupStatus.activeFoundationDir || '').toLowerCase();
        const managedLow = (setupStatus.managedFoundationDir || '').toLowerCase();
        const useManaged = !setupStatus.foundationInstalled
          || setupStatus.foundationSource === 'dev'
          || (!!activeLow && activeLow === managedLow);
        managedRadio.checked = useManaged;
        customRadio.checked = !useManaged;
      }
    }

    document.querySelectorAll('#btnSetupInstallAll, #btnSetupGenerateConfig').forEach((btn) => {
      btn.disabled = !!setupStatus.busy || rmpInstallBusy;
    });

    renderInstallLocation();
    renderOnboarding();
    renderUpdateInfo();
    syncOpenPathButtons();
  }

  async function refreshSetupStatus() {
    try {
      const profile = $('srvProfileSelect')?.value || 'debug';
      const st = await Bridge.getSetupStatus(profile);
      renderSetupStatus(st);
      return st;
    } catch {
      return null;
    }
  }

  async function refreshUpdates({ quiet = true } = {}) {
    if (typeof Bridge.checkRuntimeUpdates !== 'function') return null;
    try {
      updateInfo = await Bridge.checkRuntimeUpdates($('srvProfileSelect')?.value || null);
      renderUpdateInfo();
      if (!quiet) Studio?.toast(I18n.t('runtime.updates_checked'));
      return updateInfo;
    } catch (err) {
      if (!quiet) Studio?.toast(I18n.t('runtime.updates_fail', { error: err.message }), true);
      return null;
    }
  }

  function showSetupProgress({ percent = null, text = null } = {}) {
    document.querySelectorAll('.js-setup-progress').forEach((wrap) => wrap.classList.remove('hidden'));
    if (typeof percent === 'number') {
      document.querySelectorAll('.js-setup-progress-bar').forEach((bar) => { bar.style.width = `${percent}%`; });
    }
    if (text !== null) {
      document.querySelectorAll('.js-setup-progress-text').forEach((el) => { el.textContent = text; });
    }
  }

  function missingRequiredComponents(st) {
    const missing = [];
    if (!st?.foundationInstalled || !st?.foundationHasOodleFeature) missing.push('foundation');
    if (!st?.oodleInstalled) missing.push('oodle');
    if (!st?.foundationHasConfig && ($('launcherGamePath')?.value || '').trim()) missing.push('config');
    return missing;
  }

  async function runSetupInstall(components) {
    const useManaged = !!$('setupUseManagedDirCheck')?.checked;
    const profile = $('srvProfileSelect')?.value || 'debug';
    showSetupProgress({ percent: 5, text: I18n.t('setup.install_start', { count: components.length }) });

    try {
      await persistFormSettings({ quiet: true });
      appendLauncherLog(I18n.t('setup.install_start', { count: components.length }));
      const report = await Bridge.installRuntimeComponents(components, useManaged, profile);
      renderSetupStatus(report.status);
      await reloadPathFieldsFromSettings();
      await refreshServerStatus();

      if (report.installed?.length) {
        const msg = I18n.t('setup.install_done', { items: report.installed.join(', ') });
        appendLauncherLog(msg);
        Studio?.toast(msg);
      }
      if (report.errors?.length) {
        const friendly = report.errors.map((e) => (String(e).includes('dev_checkout:') ? I18n.t('runtime.err_dev_checkout') : e));
        const errMsg = I18n.t('setup.install_partial_err', { error: friendly.join(' | ') });
        appendLauncherLog(`ERROR (Setup): ${report.errors.join(' | ')}`);
        Studio?.toast(errMsg, true);
      }
    } catch (err) {
      appendLauncherLog(`ERROR (Setup): ${err.message}`);
      Studio?.toast(I18n.t('setup.install_partial_err', { error: err.message }), true);
    } finally {
      showSetupProgress({ percent: 100 });
      await refreshSetupStatus();
      refreshUpdates({ quiet: true });
    }
  }

  async function runComponentUpdate(component) {
    if (typeof Bridge.updateRuntimeComponent !== 'function') {
      await runSetupInstall([component]);
      return;
    }
    const name = I18n.t(COMPONENT_UI[component]?.nameKey || 'runtime.title');
    showSetupProgress({ percent: 5, text: I18n.t('runtime.updating', { component: name }) });
    try {
      await persistFormSettings({ quiet: true });
      const res = await Bridge.updateRuntimeComponent(component, $('srvProfileSelect')?.value || null);
      if (res?.status) renderSetupStatus(res.status);
      await reloadPathFieldsFromSettings();
      const msg = I18n.t('runtime.updated', { component: name, version: res?.current || '' });
      appendLauncherLog(msg);
      Studio?.toast(msg);
    } catch (err) {
      const text = String(err.message || err).includes('dev_checkout:') ? I18n.t('runtime.err_dev_checkout') : err.message;
      appendLauncherLog(`ERROR (Update ${component}): ${err.message}`);
      Studio?.toast(I18n.t('runtime.update_fail', { component: name, error: text }), true);
    } finally {
      showSetupProgress({ percent: 100 });
      await refreshSetupStatus();
      await refreshServerStatus();
      refreshUpdates({ quiet: true });
    }
  }

  function stripExeFromPath(p) {
    const trimmed = (p || '').trim().replace(/^"|"$/g, '');
    if (/\.exe$/i.test(trimmed)) return trimmed.replace(/[\\/][^\\/]+$/, '');
    return trimmed;
  }

  async function runRemakePlaceInstall(targetDir, repair) {
    if (typeof Bridge.installRemakePlace !== 'function') {
      Studio?.toast(I18n.t('runtime.rmp_fail', { error: 'installRemakePlace unavailable' }), true);
      return;
    }
    const dir = stripExeFromPath(targetDir);
    if (!dir) return;
    if (rmpStatus.running) {
      Studio?.toast(I18n.t('runtime.rmp_stop_sync_first'), true);
      return;
    }
    rmpInstallBusy = true;
    renderSetupStatus(setupStatus);
    showSetupProgress({ percent: 0, text: I18n.t('runtime.rmp_installing') });
    appendLauncherLog(I18n.t('runtime.rmp_installing'));
    try {
      await persistFormSettings({ quiet: true });
      const res = await Bridge.installRemakePlace(dir, !!repair, $('srvProfileSelect')?.value || null);
      await reloadPathFieldsFromSettings();
      const version = res?.version || res?.tag || res?.status?.remakeplaceInstalledVersion || '';
      const msg = I18n.t('runtime.rmp_done', { version });
      appendLauncherLog(msg);
      Studio?.toast(msg);
    } catch (err) {
      const msg = I18n.t('runtime.rmp_fail', { error: err.message });
      appendLauncherLog(`ERROR (ReMakePlace): ${err.message}`);
      Studio?.toast(msg, true);
    } finally {
      rmpInstallBusy = false;
      await refreshSetupStatus();
      refreshUpdates({ quiet: true });
    }
  }

  async function pickRemakePlaceInstallDir() {
    const current = stripExeFromPath($('rmpPathInput').value) || null;
    return await Bridge.pickFolder(I18n.t('runtime.rmp_pick_target'), current);
  }

  async function useExistingRemakePlace(pathHint) {
    let picked = (pathHint || '').trim();
    if (!picked) {
      picked = await Bridge.pickFile(
        I18n.t('dialog.pick_rmp'),
        $('rmpPathInput').value.trim() || null,
        I18n.t('dialog.filter_rmp'),
        ['exe']
      );
    }
    if (!picked) return;
    try {
      const resolved = await Bridge.resolveReMakePlacePath(picked);
      $('rmpPathInput').value = resolved || picked;
      renderRmpPathLabels();
      await persistFormSettings({ quiet: true });
      appendLauncherLog(I18n.t('rmp.path_saved', { path: $('rmpPathInput').value }));
      await refreshSetupStatus();
    } catch (err) {
      Studio?.toast(I18n.t('rmp.path_fail', { error: err.message }), true);
    }
  }

  // ---- Server console ----

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

  async function applyGamePath(picked) {
    $('launcherGamePath').value = picked;
    $('onbGamePath').value = picked;
    await refreshGameVersion({ quiet: false });
    await persistFormSettings({ quiet: true });
    await refreshSetupStatus();
    await I18n.refreshGameDataInfo().catch(() => null);
    renderGameDataLocaleHint();
    syncOpenPathButtons();
  }

  async function pickGamePath(current) {
    return await Bridge.pickFile(
      I18n.t('dialog.pick_game'),
      current || null,
      I18n.t('dialog.filter_ffxiv'),
      ['exe']
    );
  }

  function bindEvents() {
    $('serverStatusPill')?.addEventListener('click', () => Studio?.switchTab('home'));
    $('rmpStatusPill')?.addEventListener('click', () => Studio?.switchTab('home'));

    $('prefUiLocaleSelect')?.addEventListener('change', async (ev) => {
      try {
        await I18n.setLocale(ev.target.value);
        if (I18n.gamePreference() === 'follow_ui') {
          await I18n.refreshGameDataInfo().catch(() => null);
        }
        renderGameDataLocaleHint();
      } catch (err) {
        Studio?.toast(I18n.t('launcher.settings_save_fail', { error: err.message }), true);
      }
    });

    $('prefGameDataLocaleSelect')?.addEventListener('change', async (ev) => {
      try {
        await I18n.setGameDataLocale(ev.target.value, Number($('launcherLanguage')?.value) || 0);
        renderGameDataLocaleHint();
      } catch (err) {
        Studio?.toast(I18n.t('launcher.settings_save_fail', { error: err.message }), true);
      }
    });

    $('launcherLanguage')?.addEventListener('change', async (ev) => {
      try {
        await persistFormSettings({ quiet: true });
        I18n.syncClientLanguage(Number(ev.target.value) || 0);
        await I18n.refreshGameDataInfo().catch(() => null);
        renderGameDataLocaleHint();
      } catch {
        // Non-fatal; language selection remains active.
      }
    });

    ['launcherGamePath', 'srvFoundationRoot', 'rmpPathInput', 'prefKoGameDataPath'].forEach((id) => {
      $(id)?.addEventListener('input', syncOpenPathButtons);
    });

    const syncKoGameDataPath = async () => {
      try {
        await persistFormSettings({ quiet: true });
        await I18n.refreshGameDataInfo().catch(() => null);
        renderGameDataLocaleHint();
        syncOpenPathButtons();
      } catch (err) {
        Studio?.toast(I18n.t('launcher.settings_save_fail', { error: err.message }), true);
      }
    };

    $('prefKoGameDataPath')?.addEventListener('change', syncKoGameDataPath);
    $('btnBrowseKoGameDataPath')?.addEventListener('click', async () => {
      const picked = await Bridge.pickFolder(
        I18n.t('dialog.pick_ko_game_data'),
        $('prefKoGameDataPath')?.value.trim() || null
      );
      if (!picked || !$('prefKoGameDataPath')) return;
      $('prefKoGameDataPath').value = picked;
      await syncKoGameDataPath();
    });
    $('btnSettingsOpenKoGameData')?.addEventListener('click', () => openPathInExplorer($('prefKoGameDataPath')?.value));

    $('btnOpenFoundationDir')?.addEventListener('click', () => openPathInExplorer(activeFoundationDir()));
    $('btnOpenConfigFile')?.addEventListener('click', () => openPathInExplorer(activeConfigPath()));
    $('btnOpenRmpDir')?.addEventListener('click', () => openPathInExplorer($('rmpPathInput')?.value));
    $('btnOpenBridgeDir')?.addEventListener('click', () => openPathInExplorer(setupStatus?.activeBridgeDir));
    $('btnOpenManagedDir')?.addEventListener('click', (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      openPathInExplorer(managedRuntimeRootDir());
    });
    $('btnSettingsOpenGame')?.addEventListener('click', () => openPathInExplorer($('launcherGamePath')?.value));
    $('btnSettingsOpenFoundation')?.addEventListener('click', () => openPathInExplorer(activeFoundationDir()));
    $('btnSettingsOpenConfig')?.addEventListener('click', () => openPathInExplorer(activeConfigPath()));
    $('btnSettingsOpenRmp')?.addEventListener('click', () => openPathInExplorer($('rmpPathInput')?.value));

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
    $('launcherGamePath')?.addEventListener('blur', async () => {
      $('onbGamePath').value = $('launcherGamePath').value;
      await refreshGameVersion({ quiet: true });
    });

    $('btnBrowseGamePath')?.addEventListener('click', async () => {
      const picked = await pickGamePath($('launcherGamePath').value.trim());
      if (picked) await applyGamePath(picked);
    });

    // Onboarding
    $('btnOnbBrowseGame')?.addEventListener('click', async () => {
      const picked = await pickGamePath($('onbGamePath').value.trim());
      if (picked) await applyGamePath(picked);
    });
    $('onbGamePath')?.addEventListener('change', async (ev) => {
      const val = ev.target.value.trim();
      if (val) await applyGamePath(val);
    });
    $('btnOnbInstall')?.addEventListener('click', async () => {
      const st = (await refreshSetupStatus()) || setupStatus;
      const missing = missingRequiredComponents(st);
      if (!missing.length) {
        Studio?.toast(I18n.t('setup.install_none_missing'));
        return;
      }
      await runSetupInstall(missing);
    });
    $('btnOnbBrowseRmpDir')?.addEventListener('click', async () => {
      const picked = await Bridge.pickFolder(I18n.t('runtime.rmp_pick_target'), $('onbRmpDir').value.trim() || null);
      if (picked) $('onbRmpDir').value = picked;
    });
    $('btnOnbRmpInstall')?.addEventListener('click', async () => {
      let dir = $('onbRmpDir').value.trim();
      if (!dir) {
        dir = await pickRemakePlaceInstallDir();
        if (!dir) return;
        $('onbRmpDir').value = dir;
      }
      await runRemakePlaceInstall(dir, false);
    });
    $('btnOnbRmpExisting')?.addEventListener('click', () => useExistingRemakePlace($('onbRmpDir').value.trim()));
    $('btnOnbStart')?.addEventListener('click', () => {
      markOnboardingDone();
      renderOnboarding();
      Studio?.switchTab('home');
    });

    $('btnHomeAdvancedLaunch')?.addEventListener('click', () => Studio?.switchTab('settings'));

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
        renderRmpPathLabels();
        await refreshSetupStatus();
        await I18n.refreshGameDataInfo().catch(() => null);
        renderGameDataLocaleHint();
        syncOpenPathButtons();
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
      btn.dataset.busy = '1';
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
        appendLauncherLog(I18n.t('launcher.launch_log_pid', { pid: res.pid, path: res.executablePath }));
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
        delete btn.dataset.busy;
        renderServerStatus(serverStatus);
      }
    });

    $('btnBrowseRmpPath')?.addEventListener('click', () => useExistingRemakePlace(''));
    $('btnRmpChangeDir')?.addEventListener('click', () => useExistingRemakePlace(''));

    $('btnRmpInstall')?.addEventListener('click', async () => {
      let dir = setupStatus?.remakeplaceValid ? stripExeFromPath($('rmpPathInput').value) : '';
      if (!dir) dir = await pickRemakePlaceInstallDir();
      if (dir) await runRemakePlaceInstall(dir, false);
    });
    $('btnRmpRepair')?.addEventListener('click', async () => {
      const dir = stripExeFromPath($('rmpPathInput').value);
      if (dir) await runRemakePlaceInstall(dir, true);
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
        Studio?.switchTab('runtime');
        return;
      }
      try {
        await persistFormSettings({ quiet: true });
        appendLauncherLog(I18n.t('rmp.starting_log', { path: rmpPath }));
        const st = await Bridge.startRmpSync(rmpPath);
        renderRmpStatus(st);
        Studio?.pushExternalLog('rmp_started', 'rmp', I18n.t('rmp.started_log', { pid: st.shadowPid }));
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
      await refreshSetupStatus();
      await refreshServerStatus();
    });

    $('srvProfileSelect')?.addEventListener('change', async () => {
      await refreshServerStatus();
      await refreshSetupStatus();
    });
    $('btnServerRefresh')?.addEventListener('click', async () => {
      await persistFormSettings({ quiet: true });
      await refreshServerStatus();
    });

    $('btnServerStart')?.addEventListener('click', async () => {
      const btn = $('btnServerStart');
      btn.dataset.busy = '1';
      btn.disabled = true;
      try {
        await persistFormSettings({ quiet: true });
        const root = $('srvFoundationRoot').value.trim();
        const profile = $('srvProfileSelect').value;
        appendLauncherLog(I18n.t('server.starting', { profile }));
        const st = await Bridge.startFoundationServer(root, profile);
        renderServerStatus(st);
        Studio?.pushExternalLog('server_started', 'server', I18n.t('server.started_log', { pid: st.pid ?? '-' }));
        Studio?.toast(I18n.t('server.started_toast'));
      } catch (err) {
        appendLauncherLog(`ERROR (Server): ${err.message}`);
        Studio?.toast(I18n.t('server.start_fail', { error: err.message }), true);
      } finally {
        delete btn.dataset.busy;
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

    $('btnSetupRefresh')?.addEventListener('click', async () => {
      await persistFormSettings({ quiet: true });
      await refreshSetupStatus();
      await refreshServerStatus();
    });

    $('btnCheckUpdates')?.addEventListener('click', async () => {
      const btn = $('btnCheckUpdates');
      btn.disabled = true;
      try {
        await refreshUpdates({ quiet: false });
      } finally {
        btn.disabled = false;
      }
    });

    $('btnSetupInstallAll')?.addEventListener('click', async () => {
      const st = (await refreshSetupStatus()) || setupStatus;
      const missing = missingRequiredComponents(st);
      if (!missing.length) {
        Studio?.toast(I18n.t('setup.install_none_missing'));
        return;
      }
      await runSetupInstall(missing);
    });

    document.querySelectorAll('[data-setup-install]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const comp = btn.dataset.setupInstall;
        if (btn.dataset.mode === 'update') runComponentUpdate(comp);
        else runSetupInstall([comp]);
      });
    });

    $('btnSetupGenerateConfig')?.addEventListener('click', async () => {
      try {
        await persistFormSettings({ quiet: true });
        const root = $('srvFoundationRoot').value.trim() || null;
        const gamePath = $('launcherGamePath').value.trim() || null;
        const cfgPath = await Bridge.generateFoundationConfig(root, gamePath, true);
        await reloadPathFieldsFromSettings();
        const msg = I18n.t('setup.cfg_generated', { path: cfgPath });
        appendLauncherLog(msg);
        Studio?.toast(msg);
        await refreshSetupStatus();
        await refreshServerStatus();
      } catch (err) {
        Studio?.toast(I18n.t('setup.cfg_fail', { error: err.message }), true);
      }
    });
  }

  function subscribeBackendEvents() {
    Bridge.listen('setup-progress', (ev) => {
      const p = ev.payload;
      if (!p) return;
      showSetupProgress({
        percent: typeof p.percent === 'number' ? p.percent : null,
        text: I18n.t('setup.progress_fmt', {
          component: p.component,
          phase: p.phase,
          detail: p.detail,
        }),
      });
    });
    Bridge.listen('local-account', (ev) => {
      const p = ev.payload;
      if (!p) return;
      if (p.error) {
        appendLauncherLog(I18n.t('account.fail', { error: p.error }));
        Studio?.toast(I18n.t('account.fail', { error: p.error }), true);
      } else if (p.registered && !p.alreadyDone) {
        const msg = I18n.t('account.registered', { username: p.username || '' });
        appendLauncherLog(msg);
        Studio?.toast(msg);
      }
    });
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
    await I18n.init(
      settings?.uiLocale || 'auto',
      settings?.gameDataLocale || 'follow_client',
      Number(settings?.language) || 0
    );

    I18n.onChange(() => {
      renderPinState(alwaysOnTop);
      renderEndpoints(lastEndpoints);
      renderRmpStatus(rmpStatus);
      renderServerStatus(serverStatus);
      renderSetupStatus(setupStatus);
      renderRmpPathLabels();
      renderGameDataLocaleHint();
      renderServerConsole();
    });

    bindEvents();
    subscribeBackendEvents();

    currentShortcut = settings.globalShortcut || 'Ctrl+Shift+H';
    $('launcherServerUrl').value = settings.serverUrl || 'http://127.0.0.1:21062';
    $('launcherGamePath').value = settings.gamePath || '';
    $('onbGamePath').value = settings.gamePath || '';
    $('launcherUsername').value = settings.username || '';
    $('launcherPassword').value = settings.password || '';
    $('launcherLanguage').value = String(settings.language ?? 0);
    $('launcherExpansion').value = String(settings.expansionLevel ?? 5);
    $('launcherExtraArgs').value = settings.extraArguments || '';
    $('launcherFrontierCheck').checked = settings.includeFrontierHost !== false;
    $('launcherOpenDashboardCheck').checked = settings.openDashboardOnLaunch !== false;
    $('launcherCloseOnLaunchCheck').checked = !!settings.closeOnLaunch;
    $('rmpPathInput').value = settings.remakeplacePath || '';
    $('onbRmpDir').value = stripExeFromPath(settings.remakeplacePath || '');
    $('srvFoundationRoot').value = settings.foundationRoot || '';
    $('prefShortcutInput').value = currentShortcut;
    $('prefMinimizeToTrayCheck').checked = settings.minimizeToTrayOnClose !== false;
    if ($('prefGameDataLocaleSelect')) {
      $('prefGameDataLocaleSelect').value = settings.gameDataLocale || 'follow_client';
    }
    if ($('prefKoGameDataPath')) {
      $('prefKoGameDataPath').value = settings.koGameDataPath || '';
    }
    renderPinState(!!settings.alwaysOnTop);
    renderRmpPathLabels();

    Studio?.syncWatchedSettings(settings.watchedPresetDirs, settings.autoApplyWatchedPresets);
    await Studio?.refreshLocalPresets();

    await refreshGameVersion({ quiet: true });

    const [rmp, , setupSt, existingLogs] = await Promise.all([
      Bridge.getRmpStatus().catch(() => null),
      refreshServerStatus(),
      refreshSetupStatus(),
      Bridge.getServerLogs().catch(() => []),
      I18n.refreshGameDataInfo().catch(() => null),
    ]);
    renderGameDataLocaleHint();
    syncOpenPathButtons();
    if (rmp) renderRmpStatus(rmp);
    if (setupSt) renderSetupStatus(setupSt);
    if (Array.isArray(existingLogs) && existingLogs.length) {
      existingLogs.forEach(appendServerLogEntry);
    }

    appendLauncherLog(I18n.t('launcher.ready'));
    if (setupSt && (!requiredReady(setupSt) || !setupSt.gamePathValid)) {
      Studio?.switchTab('home');
    }

    refreshUpdates({ quiet: true });

    setInterval(() => {
      refreshServerStatus();
    }, 5000);
  }

  init().catch((err) => {
    console.error('Launcher init error:', err);
  });
})();
