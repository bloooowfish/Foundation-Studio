(() => {
  'use strict';

  const STORAGE_KEY = 'foundation_housing_studio_prefs_v1';
  const MAX_LOG_ENTRIES = 300;
  const Bridge = window.FoundationBridge;
  const I18n = window.FoundationI18n;

  const state = {
    activeTab: 'launcher',
    viewMode: 'cards',
    favoritesOnly: false,
    favorites: {},
    memos: {},
    primarySession: null,
    estates: [],
    selectedLandIdent: null,
    selectedEstateDetail: null,
    serverPresets: [],
    localPresets: [],
    watchedPresets: [],
    watchedDirs: [],
    autoApplyWatched: false,
    selectedPresetId: null,
    bridgeSnapshot: null,
    furnScope: 'indoor',
    selectedFurnSlot: null,
    transformDraft: null,
    transformOriginal: null,
    transformDirty: false,
    envDirty: { light: false, style: false, meta: false, memo: false },
    pendingConfirm: false,
    detailReqSeq: 0,
    bridgeReqSeq: 0,
    liveMoveTimer: null,
    sseRefreshTimer: null,
    logs: [],
    logCategory: 'all',
    sseConnected: false,
  };

  const $ = (id) => document.getElementById(id);

  function resetEnvDirty() {
    state.envDirty.light = false;
    state.envDirty.style = false;
    state.envDirty.meta = false;
    state.envDirty.memo = false;
  }

  function cancelPendingLiveMove() {
    if (state.liveMoveTimer !== null) {
      clearTimeout(state.liveMoveTimer);
      state.liveMoveTimer = null;
    }
  }

  function wrapDegrees(deg) {
    const num = Number(deg);
    if (!Number.isFinite(num)) return 0;
    const wrapped = ((((num + 180) % 360) + 360) % 360) - 180;
    return (wrapped === -180 && num > 0) ? 180 : wrapped;
  }

  async function runBusy(el, fn) {
    if (el && el.dataset) {
      if (el.dataset.busy === '1') return;
      el.dataset.busy = '1';
      el.disabled = true;
    }
    try {
      return await fn();
    } finally {
      if (el && el.dataset) {
        delete el.dataset.busy;
        el.disabled = false;
      }
      renderSidebarSelected();
      syncEnvironmentButtonStates(getSelectedEstate());
    }
  }

  function loadPrefs() {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === 'object') {
        if (parsed.activeTab) state.activeTab = parsed.activeTab;
        if (parsed.viewMode === 'table' || parsed.viewMode === 'cards') state.viewMode = parsed.viewMode;
        if (parsed.favorites && typeof parsed.favorites === 'object') state.favorites = parsed.favorites;
        if (parsed.memos && typeof parsed.memos === 'object') state.memos = parsed.memos;
      }
    } catch {
      // Ignore corrupt localStorage entries.
    }
  }

  function savePrefs() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({
        activeTab: state.activeTab,
        viewMode: state.viewMode,
        favorites: state.favorites,
        memos: state.memos,
      }));
    } catch {
      // Ignore storage quota issues.
    }
  }

  function esc(value) {
    return String(value ?? '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function utf8ByteLength(str) {
    return new TextEncoder().encode(str || '').length;
  }

  function toast(message, isError = false) {
    const stack = $('toastStack');
    const el = document.createElement('div');
    el.className = `toast ${isError ? 'err' : 'ok'}`;
    el.textContent = message;
    stack.appendChild(el);
    setTimeout(() => {
      el.remove();
    }, 3600);
  }

  function askConfirm(title, message) {
    if (state.pendingConfirm) {
      return Promise.resolve(false);
    }
    state.pendingConfirm = true;
    const prevFocus = document.activeElement;
    return new Promise((resolve) => {
      const modal = $('confirmModal');
      $('confirmModalTitle').textContent = title;
      $('confirmModalMessage').textContent = message;
      modal.classList.remove('hidden');

      const cleanup = (answer) => {
        modal.classList.add('hidden');
        $('confirmModalOk').removeEventListener('click', onOk);
        $('confirmModalCancel').removeEventListener('click', onCancel);
        modal.removeEventListener('click', onBackdrop);
        document.removeEventListener('keydown', onKeyDown);
        state.pendingConfirm = false;
        if (prevFocus && typeof prevFocus.focus === 'function' && prevFocus.isConnected) {
          prevFocus.focus();
        }
        resolve(answer);
      };
      const onOk = () => cleanup(true);
      const onCancel = () => cleanup(false);
      const onBackdrop = (ev) => {
        if (ev.target === modal) cleanup(false);
      };
      const onKeyDown = (ev) => {
        if (ev.key === 'Escape') {
          ev.preventDefault();
          cleanup(false);
        }
      };

      $('confirmModalOk').addEventListener('click', onOk);
      $('confirmModalCancel').addEventListener('click', onCancel);
      modal.addEventListener('click', onBackdrop);
      document.addEventListener('keydown', onKeyDown);
      $('confirmModalCancel').focus();
    });
  }

  async function apiJson(path, options = {}) {
    return await Bridge.apiRequest(path, options);
  }

  function formatDistrictName(tid) {
    return I18n.formatDistrict(tid);
  }

  function formatSizeLabel(plotSize, isApartment) {
    return I18n.formatSize(plotSize, isApartment);
  }

  function formatSizeCapacity(plotSize, isApartment) {
    if (isApartment) return { indoor: 100, outdoor: 0 };
    if (plotSize === 0) return { indoor: 300, outdoor: 40 };
    if (plotSize === 1) return { indoor: 450, outdoor: 60 };
    if (plotSize === 2) return { indoor: 600, outdoor: 80 };
    return { indoor: null, outdoor: null };
  }

  function formatStyleName(raw, fallbackKey = 'game.styles.default') {
    return I18n.formatStyle(raw, fallbackKey);
  }

  function normalizeStyleKey(raw) {
    return I18n.normalizeStyleSelectValue(raw);
  }

  function formatEstateLocation(row) {
    return I18n.formatEstateLocation(row);
  }

  function formatTimestamp(sec) {
    if (!sec) return '-';
    try {
      return new Date(sec * 1000).toLocaleString(I18n.bcp47());
    } catch {
      return String(sec);
    }
  }

  function getSelectedEstate() {
    return state.estates.find((e) => e.row.land_ident === state.selectedLandIdent) || null;
  }

  function getAllPresets() {
    const seen = new Set();
    const out = [];
    for (const p of [...state.watchedPresets, ...state.localPresets, ...state.serverPresets]) {
      if (seen.has(p.id)) continue;
      seen.add(p.id);
      out.push(p);
    }
    return out;
  }

  function getPresetSizeText(preset) {
    if (preset.parseError) return I18n.t('presets.detail_parse_error');
    return I18n.formatSizeKey(preset.sizeKey, preset.houseSizeRaw);
  }

  function getPresetStyleText(preset) {
    if (preset.parseError) return I18n.t('presets.detail_parse_error');
    return I18n.formatStyle(
      preset.styleKey || preset.districtRaw,
      'game.styles.unspecified',
      preset.districtRaw
    );
  }

  function getPresetSourceLabel(preset) {
    return I18n.t(`presets.source_${preset.sourceKindKey || 'local'}`);
  }

  function mapWatchedPresetEntry(entry) {
    const meta = entry.meta || {};
    return {
      id: `disk:${entry.path}`,
      sourceKindKey: 'watched',
      diskPath: entry.path,
      dir: entry.dir,
      name: entry.fileName,
      title: entry.title || entry.fileName.replace(/\.json$/i, ''),
      rawJson: null,
      serverPath: null,
      houseSizeRaw: meta.houseSizeRaw || '',
      normalizedPlotSize: meta.normalizedPlotSize ?? null,
      isApartment: !!meta.isApartment,
      sizeKey: meta.sizeKey || 'unknown',
      districtRaw: meta.districtRaw || '',
      styleKey: meta.styleKey || 'unspecified',
      indoorCount: meta.indoorCount || 0,
      indoorDyed: meta.indoorDyed || 0,
      indoorMaterial: meta.indoorMaterial || 0,
      outdoorCount: meta.outdoorCount || 0,
      outdoorDyed: meta.outdoorDyed || 0,
      interiorFixtures: Array.isArray(meta.interiorFixtures) ? meta.interiorFixtures : [],
      exteriorFixtures: Array.isArray(meta.exteriorFixtures) ? meta.exteriorFixtures : [],
      interiorScale: Number(meta.interiorScale ?? 1.0),
      exteriorScale: Number(meta.exteriorScale ?? 1.0),
      modifiedUnixMs: entry.modifiedUnixMs || 0,
      parseError: entry.parseError || null,
    };
  }

  async function refreshLocalPresets() {
    try {
      const entries = await Bridge.scanLocalPresets();
      state.watchedPresets = Array.isArray(entries) ? entries.map(mapWatchedPresetEntry) : [];
      const all = getAllPresets();
      if (!all.some((p) => p.id === state.selectedPresetId)) {
        state.selectedPresetId = all[0]?.id ?? null;
      }
      renderPresets();
      renderSidebarSelected();
    } catch (err) {
      toast(I18n.t('presets.scan_fail', { error: err.message }), true);
    }
  }

  function renderWatchedDirs() {
    const el = $('watchedDirsList');
    if (!el) return;
    if (!state.watchedDirs.length) {
      el.innerHTML = `<span class="muted small-text">${esc(I18n.t('presets.watch_empty'))}</span>`;
      return;
    }
    const openTitle = esc(I18n.t('presets.watch_open_title'));
    const removeTitle = esc(I18n.t('presets.watch_remove_title'));
    el.innerHTML = state.watchedDirs.map((dir) => `
      <span class="watch-chip">
        <button type="button" data-open-watch="${esc(dir)}" title="${openTitle}">${esc(dir)}</button>
        <button type="button" data-remove-watch="${esc(dir)}" title="${removeTitle}">×</button>
      </span>
    `).join('');
  }

  function getSelectedPreset() {
    const all = getAllPresets();
    return all.find((p) => p.id === state.selectedPresetId) || null;
  }

  function decodeBufferWithBom(buffer) {
    const bytes = new Uint8Array(buffer);
    if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) {
      return new TextDecoder('utf-16le').decode(bytes.subarray(2));
    }
    if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) {
      return new TextDecoder('utf-16be').decode(bytes.subarray(2));
    }
    if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
      return new TextDecoder('utf-8').decode(bytes.subarray(3));
    }
    return new TextDecoder('utf-8').decode(bytes);
  }

  function normalizePresetSize(rawSize) {
    const s = String(rawSize || '').trim().toLowerCase();
    if (!s) return { plotSize: null, isApartment: false, sizeKey: 'unknown' };
    if (/\b(apart(ment)?|chamber|fc\s*room|private\s*room|room)\b/i.test(s) || s === 'apt') {
      return { plotSize: 0, isApartment: true, sizeKey: 'apartment' };
    }
    if (s === 'l' || s.includes('large') || s.includes('mansion')) {
      return { plotSize: 2, isApartment: false, sizeKey: 'large' };
    }
    if (s === 's' || s.includes('small') || s.includes('cottage')) {
      return { plotSize: 0, isApartment: false, sizeKey: 'small' };
    }
    if (s === 'm' || s.includes('medium') || s.includes('house')) {
      return { plotSize: 1, isApartment: false, sizeKey: 'medium' };
    }
    return { plotSize: null, isApartment: false, sizeKey: 'other' };
  }

  function analyzeFurnitureArray(arr) {
    let total = 0;
    let dyed = 0;
    let material = 0;
    const visit = (item) => {
      if (!item || typeof item !== 'object') return;
      total += 1;
      const props = item.properties;
      if (props && typeof props === 'object') {
        if ((typeof props.color === 'string' && props.color.trim()) || (typeof props.stainId === 'number' && props.stainId > 0)) {
          dyed += 1;
        }
        if (props.material && typeof props.material === 'object' && props.material.itemId > 0) {
          material += 1;
        }
      }
      if (Array.isArray(item.attachments)) {
        item.attachments.forEach(visit);
      }
    };
    if (Array.isArray(arr)) arr.forEach(visit);
    return { total, dyed, material };
  }

  function extractFixtureEntries(arr) {
    if (!Array.isArray(arr)) return [];
    const out = [];
    for (const f of arr) {
      if (!f || typeof f !== 'object') continue;
      const kind = String(f.type || '').trim();
      const name = String(f.name || '').trim();
      if (!kind || !name) continue;
      const lower = kind.toLowerCase();
      if (lower === 'side door' || lower.includes('hidden') || lower === 'district') continue;
      out.push({
        kindKey: I18n.mapFixtureKindToKey(kind),
        rawKind: kind,
        name,
        color: f.color ? String(f.color).trim() : '',
      });
    }
    return out;
  }

  function parseLocalPresetFile(fileName, rawJson, lastModifiedMs, relativePath = '') {
    const key = relativePath || fileName;
    const id = `local:${key}`;
    const title = key.replace(/\.json$/i, '');
    try {
      const parsed = JSON.parse(rawJson);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new Error('JSON root must be an object ({}).');
      }
      const sizeInfo = normalizePresetSize(parsed.houseSize);
      const intFixtures = Array.isArray(parsed.interiorFixture) ? parsed.interiorFixture : [];
      const extFixtures = Array.isArray(parsed.exteriorFixture) ? parsed.exteriorFixture : [];
      const districtFixture = intFixtures.find((f) => String(f?.type || '').toLowerCase() === 'district');
      const districtRaw = districtFixture?.name ? String(districtFixture.name).trim() : '';
      const styleKey = I18n.canonicalStyleKey(districtRaw) || (districtRaw ? 'unknown' : 'unspecified');
      const indoorStats = analyzeFurnitureArray(parsed.interiorFurniture);
      const outdoorStats = analyzeFurnitureArray(parsed.exteriorFurniture);

      return {
        id,
        sourceKindKey: 'local',
        name: fileName,
        title,
        rawJson,
        serverPath: null,
        houseSizeRaw: parsed.houseSize || '',
        normalizedPlotSize: sizeInfo.plotSize,
        isApartment: sizeInfo.isApartment,
        sizeKey: sizeInfo.sizeKey,
        districtRaw,
        styleKey,
        indoorCount: indoorStats.total,
        indoorDyed: indoorStats.dyed,
        indoorMaterial: indoorStats.material,
        outdoorCount: outdoorStats.total,
        outdoorDyed: outdoorStats.dyed,
        interiorFixtures: extractFixtureEntries(intFixtures),
        exteriorFixtures: extractFixtureEntries(extFixtures),
        interiorScale: Number(parsed.interiorScale ?? 1.0),
        exteriorScale: Number(parsed.exteriorScale ?? 1.0),
        modifiedUnixMs: lastModifiedMs || Date.now(),
        parseError: null,
      };
    } catch (err) {
      return {
        id,
        sourceKindKey: 'local',
        name: fileName,
        title,
        rawJson,
        serverPath: null,
        houseSizeRaw: '',
        sizeKey: 'unknown',
        normalizedPlotSize: null,
        isApartment: false,
        districtRaw: '',
        styleKey: 'unspecified',
        indoorCount: 0,
        indoorDyed: 0,
        indoorMaterial: 0,
        outdoorCount: 0,
        outdoorDyed: 0,
        interiorFixtures: [],
        exteriorFixtures: [],
        interiorScale: 1,
        exteriorScale: 1,
        modifiedUnixMs: lastModifiedMs || Date.now(),
        parseError: err.message,
      };
    }
  }

  function mapServerPreset(summary) {
    const sizeInfo = normalizePresetSize(summary.house_size);
    const districtRaw = summary.district ? String(summary.district).trim() : '';
    const styleKey = I18n.canonicalStyleKey(districtRaw) || (districtRaw ? 'unknown' : 'unspecified');
    return {
      id: `server:${summary.path}`,
      sourceKindKey: 'server',
      name: summary.name,
      title: String(summary.name || '').replace(/\.json$/i, ''),
      rawJson: null,
      serverPath: summary.path,
      houseSizeRaw: summary.house_size || '',
      normalizedPlotSize: sizeInfo.plotSize,
      isApartment: sizeInfo.isApartment,
      sizeKey: sizeInfo.sizeKey,
      districtRaw,
      styleKey,
      indoorCount: summary.indoor_count || 0,
      indoorDyed: 0,
      indoorMaterial: 0,
      outdoorCount: summary.outdoor_count || 0,
      outdoorDyed: 0,
      interiorFixtures: summary.interior_fixtures || [],
      exteriorFixtures: summary.exterior_fixtures || [],
      interiorScale: 1,
      exteriorScale: 1,
      modifiedUnixMs: summary.modified_unix_ms || 0,
      parseError: null,
    };
  }

  function evaluateCompat(preset, estate) {
    if (!estate) {
      return { status: 'neutral', text: I18n.t('presets.compat_select_target'), mismatch: false };
    }
    if (!preset) {
      return { status: 'neutral', text: I18n.t('presets.compat_select_preset'), mismatch: false };
    }
    if (preset.parseError) {
      return {
        status: 'warn',
        text: I18n.t('presets.compat_parse_err', { error: preset.parseError }),
        mismatch: false,
      };
    }

    const row = estate.row;
    const targetBadge = formatSizeLabel(row.plot_size, row.is_apartment);
    const cap = formatSizeCapacity(row.plot_size, row.is_apartment);

    if (preset.normalizedPlotSize === null) {
      return {
        status: 'neutral',
        text: I18n.t('presets.compat_unknown_size', {
          target: targetBadge,
          indoor: preset.indoorCount,
          outdoor: preset.outdoorCount,
        }),
        mismatch: false,
      };
    }

    const sameType = preset.isApartment === row.is_apartment && (row.is_apartment || preset.normalizedPlotSize === row.plot_size);
    if (!sameType) {
      return {
        status: 'warn',
        mismatch: true,
        text: I18n.t('presets.compat_mismatch', {
          preset: getPresetSizeText(preset),
          target: targetBadge,
        }),
      };
    }

    if (cap.indoor !== null && preset.indoorCount > cap.indoor) {
      return {
        status: 'warn',
        mismatch: false,
        text: I18n.t('presets.compat_indoor_over', {
          target: targetBadge,
          count: preset.indoorCount,
          cap: cap.indoor,
        }),
      };
    }

    if (cap.outdoor !== null && preset.outdoorCount > cap.outdoor) {
      const key = row.is_apartment
        ? 'presets.compat_outdoor_over_apt'
        : 'presets.compat_outdoor_over';
      return {
        status: 'warn',
        mismatch: false,
        text: I18n.t(key, {
          target: targetBadge,
          count: preset.outdoorCount,
          cap: cap.outdoor,
        }),
      };
    }

    return {
      status: 'match',
      mismatch: false,
      text: I18n.t('presets.compat_ok', {
        target: targetBadge,
        indoor: preset.indoorCount,
        indoorCap: cap.indoor ?? '-',
        outdoor: preset.outdoorCount,
        outdoorCap: cap.outdoor ?? '-',
      }),
    };
  }

  // ---- Navigation & Selection ----

  function clearSelectionTransientState({ cancelTimer = true } = {}) {
    if (cancelTimer) cancelPendingLiveMove();
    state.selectedEstateDetail = null;
    state.bridgeSnapshot = null;
    state.selectedFurnSlot = null;
    state.transformDraft = null;
    state.transformOriginal = null;
    state.transformDirty = false;
    resetEnvDirty();
    $('presetPreviewReport').classList.add('hidden');
  }

  function switchTab(tabName) {
    state.activeTab = tabName;
    savePrefs();
    document.querySelectorAll('.nav-item').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.tab === tabName);
    });
    document.querySelectorAll('.tab-view').forEach((view) => {
      view.classList.toggle('active', view.id === `view-${tabName}`);
    });

    if (tabName === 'furniture' && state.selectedLandIdent) {
      refreshBridgeSnapshot({ forceSyncDraft: !state.transformDirty });
    } else if (tabName === 'environment' && state.selectedLandIdent) {
      refreshEstateDetail();
    }
  }

  async function selectEstate(landIdent, loadSubViews = true) {
    if (landIdent !== state.selectedLandIdent) {
      clearSelectionTransientState();
    }
    state.selectedLandIdent = landIdent;
    renderAll();

    if (!landIdent || !loadSubViews) return;
    if (state.activeTab === 'furniture') {
      await refreshBridgeSnapshot({ forceSyncDraft: true });
    } else if (state.activeTab === 'environment') {
      await refreshEstateDetail({ forceSync: true });
    } else {
      // Load detail silently so snapshot count stays fresh in sidebar badge.
      await refreshEstateDetail({ silent: true, forceSync: true });
    }
  }

  // ---- Data Fetching ----

  async function refreshAll(preferredLandIdent = null, { forceSync = false, silentError = false } = {}) {
    try {
      const prevIdent = state.selectedLandIdent;
      const [status, estates, presetsResp] = await Promise.all([
        apiJson('/status'),
        apiJson('/estates'),
        apiJson('/presets').catch(() => ({ files: [] })),
      ]);

      $('protocolPatchBadge').textContent = status.protocol_patch || '7.58';
      state.primarySession = Array.isArray(status.online_sessions) && status.online_sessions.length > 0
        ? status.online_sessions[0]
        : null;
      state.estates = Array.isArray(estates) ? estates : [];
      state.serverPresets = Array.isArray(presetsResp.files)
        ? presetsResp.files.map(mapServerPreset)
        : [];

      const activeIdent = state.primarySession?.active_estate?.land_ident ?? null;
      const validActiveIdent = activeIdent !== null && state.estates.some((e) => e.row.land_ident === activeIdent)
        ? activeIdent
        : null;
      const desiredIdent = preferredLandIdent
        ?? state.selectedLandIdent
        ?? validActiveIdent
        ?? state.estates[0]?.row.land_ident
        ?? null;

      if (desiredIdent !== null && state.estates.some((e) => e.row.land_ident === desiredIdent)) {
        state.selectedLandIdent = desiredIdent;
      } else {
        state.selectedLandIdent = state.estates[0]?.row.land_ident ?? null;
      }

      if (state.selectedLandIdent !== prevIdent || !state.selectedLandIdent) {
        clearSelectionTransientState();
      } else if (forceSync) {
        state.transformDirty = false;
        resetEnvDirty();
      }

      const allPresets = getAllPresets();
      if (!allPresets.some((p) => p.id === state.selectedPresetId)) {
        state.selectedPresetId = allPresets[0]?.id ?? null;
      }

      renderAll({ forceSync });

      if (state.selectedLandIdent) {
        if (state.activeTab === 'furniture') {
          await refreshBridgeSnapshot({ forceSyncDraft: forceSync || !state.transformDirty });
        } else {
          await refreshEstateDetail({ silent: true, forceSync });
        }
      } else {
        clearSelectionTransientState();
        renderAll({ forceSync: true });
      }
    } catch (err) {
      if (!silentError) {
        toast(I18n.t('estates.status_load_fail', { error: err.message }), true);
      }
    }
  }

  async function refreshEstatesOnly() {
    try {
      const prevIdent = state.selectedLandIdent;
      const estates = await apiJson('/estates');
      state.estates = Array.isArray(estates) ? estates : [];
      if (!state.estates.some((e) => e.row.land_ident === state.selectedLandIdent)) {
        const activeIdent = state.primarySession?.active_estate?.land_ident ?? null;
        const validActiveIdent = activeIdent !== null && state.estates.some((e) => e.row.land_ident === activeIdent)
          ? activeIdent
          : null;
        state.selectedLandIdent = validActiveIdent
          ?? state.estates[0]?.row.land_ident
          ?? null;
      }
      if (state.selectedLandIdent !== prevIdent || !state.selectedLandIdent) {
        clearSelectionTransientState();
      }
      renderAll();
      if (state.selectedLandIdent) {
        if (state.activeTab === 'furniture') await refreshBridgeSnapshot({ forceSyncDraft: false });
        if (state.activeTab === 'environment') await refreshEstateDetail({ silent: true, forceSync: false });
      }
    } catch (err) {
      toast(I18n.t('estates.refresh_fail', { error: err.message }), true);
    }
  }

  async function refreshEstateDetail({ silent = false, forceSync = false } = {}) {
    const reqIdent = state.selectedLandIdent;
    const seq = ++state.detailReqSeq;
    if (!reqIdent) {
      state.selectedEstateDetail = null;
      renderEnvironment({ forceSync: true });
      renderSidebarSelected();
      return;
    }
    try {
      const detail = await apiJson(`/estates/${reqIdent}`);
      if (seq !== state.detailReqSeq || state.selectedLandIdent !== reqIdent) return;
      if (detail?.estate?.land_ident === reqIdent) {
        state.selectedEstateDetail = detail;
        renderEnvironment({ forceSync });
        renderSidebarSelected();
      }
    } catch (err) {
      if (!silent) toast(I18n.t('estates.detail_fail', { error: err.message }), true);
    }
  }

  async function refreshBridgeSnapshot({ silent = false, forceSyncDraft = false } = {}) {
    const reqIdent = state.selectedLandIdent;
    const seq = ++state.bridgeReqSeq;
    if (!reqIdent) {
      state.bridgeSnapshot = null;
      renderFurniture();
      return;
    }
    try {
      const snap = await apiJson(`/estates/${reqIdent}/bridge/snapshot`);
      if (seq !== state.bridgeReqSeq || state.selectedLandIdent !== reqIdent) return;
      if (snap?.land_ident === reqIdent) {
        state.bridgeSnapshot = snap;
        syncSelectedFurnitureIntoDraft({ force: forceSyncDraft });
        renderFurniture();
      }
    } catch (err) {
      if (!silent) toast(I18n.t('estates.bridge_fail', { error: err.message }), true);
    }
  }

  // ---- SSE Connection ----

  function categorizeEventKind(kind) {
    if (!kind) return 'env';
    if (kind.startsWith('launcher_') || kind.startsWith('rmp_') || kind.startsWith('server_') || kind.startsWith('watcher_')) {
      return 'launcher';
    }
    if (kind.startsWith('session_') || kind === 'kit_granted') return 'session';
    if (kind === 'preset_applied' || kind === 'live_diff_applied') return 'preset';
    if (kind.startsWith('furniture_') || kind === 'layout_changed') return 'furniture';
    return 'env';
  }

  function pushLog(evt) {
    state.logs.unshift({
      time: new Date().toLocaleTimeString(I18n.bcp47(), { hour12: false }),
      kind: evt.kind || 'info',
      category: evt.category || categorizeEventKind(evt.kind),
      source: evt.source || '',
      revision: evt.revision ?? null,
      landIdent: evt.land_ident ?? null,
      message: evt.message || '',
    });
    if (state.logs.length > MAX_LOG_ENTRIES) {
      state.logs.length = MAX_LOG_ENTRIES;
    }
    renderLogs();
  }

  function handleSsePayload(rawJson) {
    let evt;
    try {
      evt = JSON.parse(rawJson);
    } catch {
      return;
    }
    setSseConnected(true);

    if (evt.kind === 'session_offline') {
      state.primarySession = evt.session || null;
      renderSessionHeader();
      renderEstates();
      if (evt.message) pushLog(evt);
      return;
    }

    if (evt.session) {
      state.primarySession = evt.session;
      renderSessionHeader();
      renderEstates();
    }
    if (evt.message && (evt.kind !== 'furniture_moved' || evt.source)) {
      pushLog(evt);
    } else if (evt.kind === 'furniture_moved' && !evt.session) {
      pushLog(evt);
    }

    const shouldRefreshEstates = [
      'estate_ensured',
      'estate_updated',
      'estate_reset',
      'estate_deleted',
      'preset_applied',
      'live_diff_applied',
      'snapshot_created',
      'snapshot_restored',
      'session_switched',
      'furniture_added',
      'furniture_removed',
      'layout_changed',
    ].includes(evt.kind);

    if (shouldRefreshEstates || evt.kind === 'furniture_moved' || evt.kind === 'furniture_dyed') {
      clearTimeout(state.sseRefreshTimer);
      state.sseRefreshTimer = setTimeout(() => {
        refreshEstatesOnly();
      }, 220);
    }
  }

  function setSseConnected(connected) {
    const wasConnected = state.sseConnected;
    state.sseConnected = connected;
    $('sseDot').className = `dot ${connected ? 'dot-ok' : 'dot-warn'}`;
    $('sseStatusText').textContent = I18n.t(
      connected ? 'topbar.sse_connected' : 'topbar.sse_disconnected'
    );
    if (!wasConnected && connected) {
      refreshAll(state.selectedLandIdent, { forceSync: false, silentError: true });
    }
  }

  function connectSse() {
    Bridge.subscribeSse(
      (status) => {
        setSseConnected(!!status?.connected);
      },
      (payload) => {
        if (payload?.data) {
          handleSsePayload(payload.data);
        }
      }
    );
  }

  async function applyPresetObject(preset, target) {
    if (preset.diskPath) {
      return await Bridge.applyLocalPresetFile({
        landIdent: target.row.land_ident,
        filePath: preset.diskPath,
        scope: $('presetScopeSelect').value,
        reloadMode: $('presetReloadSelect').value,
        preservePos: $('presetPreservePos').checked,
        forceEnter: false,
        label: preset.title,
      });
    }
    return await apiJson(`/estates/${target.row.land_ident}/preset/apply`, {
      method: 'POST',
      body: JSON.stringify(buildPresetRequestBody(preset)),
    });
  }

  async function previewPresetObject(preset, target) {
    if (preset.diskPath) {
      return await Bridge.previewLocalPresetFile(
        preset.diskPath,
        $('presetScopeSelect').value,
        target.row.land_ident
      );
    }
    return await apiJson('/presets/preview', {
      method: 'POST',
      body: JSON.stringify({
        ...(preset.rawJson !== null ? { raw_json: preset.rawJson } : { path: preset.serverPath }),
        scope: $('presetScopeSelect').value,
        land_ident: target.row.land_ident,
      }),
    });
  }

  function connectWatcherAndDropEvents() {
    Bridge.listen('local-presets-changed', async (ev) => {
      const payload = ev.payload;
      if (!payload) return;
      await refreshLocalPresets();
      const name = payload.fileName || payload.path;
      if (payload.kind === 'removed') {
        pushLog({
          kind: 'watcher_removed',
          source: 'watcher',
          message: I18n.t('presets.watch_removed_log', { name }),
        });
        return;
      }
      if (payload.entry) {
        const mapped = mapWatchedPresetEntry(payload.entry);
        state.selectedPresetId = mapped.id;
        renderPresets();
        pushLog({
          kind: 'watcher_updated',
          source: 'watcher',
          message: I18n.t('presets.watch_updated_log', { name }),
        });

        if (state.autoApplyWatched && !mapped.parseError) {
          const target = getSelectedEstate();
          if (!target) {
            toast(I18n.t('presets.auto_skip_no_target', { name }), true);
            return;
          }
          const compat = evaluateCompat(mapped, target);
          if (compat.mismatch) {
            toast(I18n.t('presets.auto_skip_mismatch', { name, reason: compat.text }), true);
            return;
          }
          try {
            const res = await applyPresetObject(mapped, target);
            toast(
              I18n.t('presets.auto_apply_ok', {
                name,
                indoor: res.preview.interior_resolved_count,
                outdoor: res.preview.exterior_resolved_count,
              })
            );
            await refreshAll(target.row.land_ident, { forceSync: true, silentError: true });
          } catch (err) {
            toast(I18n.t('presets.auto_apply_fail', { name, error: err.message }), true);
          }
        } else {
          toast(I18n.t('presets.watch_updated_toast', { name }));
        }
      }
    });

    Bridge.onDragDrop(
      async (payload) => {
        $('presetDropZone')?.classList.remove('dragover');
        const paths = Array.isArray(payload?.paths) ? payload.paths : [];
        if (!paths.length) return;

        let changed = false;
        let selectedId = null;
        for (const p of paths) {
          if (String(p).toLowerCase().endsWith('.json')) {
            try {
              const entry = await Bridge.inspectPresetFile(p);
              const mapped = mapWatchedPresetEntry(entry);
              mapped.sourceKindKey = 'local';
              const idx = state.localPresets.findIndex((x) => x.id === mapped.id);
              if (idx >= 0) state.localPresets[idx] = mapped;
              else state.localPresets.unshift(mapped);
              selectedId = mapped.id;
              changed = true;
            } catch (err) {
              toast(I18n.t('presets.file_drop_err', { error: err.message }), true);
            }
          } else {
            try {
              state.watchedDirs = await Bridge.addWatchedPresetDir(p);
              renderWatchedDirs();
              await refreshLocalPresets();
              toast(I18n.t('presets.watch_added', { dir: p }));
              changed = true;
            } catch (err) {
              toast(I18n.t('presets.watch_add_fail', { error: err.message }), true);
            }
          }
        }
        if (changed) {
          if (selectedId) state.selectedPresetId = selectedId;
          if (state.activeTab !== 'presets') switchTab('presets');
          renderPresets();
          renderSidebarSelected();
        }
      },
      () => $('presetDropZone')?.classList.add('dragover'),
      () => $('presetDropZone')?.classList.remove('dragover')
    );
  }

  // ---- Renderers ----

  function renderAll({ forceSync = false } = {}) {
    renderSessionHeader();
    renderSidebarSelected();
    renderEstates();
    renderPresets();
    renderFurniture();
    renderEnvironment({ forceSync });
    renderLogs();
  }

  function renderSessionHeader() {
    const sess = state.primarySession;
    if (!sess) {
      $('sessionPlayerText').textContent = I18n.t('topbar.session_waiting');
      $('sessionEstateText').textContent = I18n.t('topbar.session_preconfig');
      return;
    }

    const zoneText = I18n.formatZone(sess.zone_id);
    $('sessionPlayerText').textContent = `${sess.character_name} · ${zoneText}`;
    if (sess.active_estate) {
      const ae = sess.active_estate;
      const loc = formatEstateLocation(ae);
      const size = formatSizeLabel(ae.plot_size, ae.is_apartment);
      $('sessionEstateText').textContent = I18n.t('topbar.session_active', {
        location: loc,
        size,
      });
    } else {
      $('sessionEstateText').textContent = I18n.t('topbar.session_outside');
    }
  }

  function renderSidebarSelected() {
    const est = getSelectedEstate();
    $('navCountEstates').textContent = state.estates.length;
    $('navCountPresets').textContent = getAllPresets().length;
    $('navCountLogs').textContent = state.logs.length;

    const totalFurn = est ? (est.indoor_count + est.outdoor_count) : 0;
    const snapCount = state.selectedEstateDetail?.estate?.land_ident === est?.row?.land_ident
      ? (state.selectedEstateDetail.snapshots?.length ?? est.snapshot_count ?? 0)
      : (est?.snapshot_count ?? 0);
    $('navCountFurniture').textContent = totalFurn;
    $('navCountSnapshots').textContent = snapCount;

    const btnIds = ['sidebarBtnEnter', 'sidebarBtnExit', 'sidebarBtnReload', 'sidebarBtnUndo'];
    if (!est) {
      $('sidebarSizeBadge').textContent = I18n.t('sidebar.unselected_badge');
      $('sidebarEstateTitle').textContent = I18n.t('sidebar.unselected_title');
      $('sidebarEstateSub').textContent = I18n.t('sidebar.unselected_sub');
      $('sidebarEstateStats').innerHTML = '';
      btnIds.forEach((id) => { $(id).disabled = true; });
      return;
    }

    const row = est.row;
    const loc = formatEstateLocation(row);
    const title = row.estate_name?.trim() ? `${row.estate_name} (${loc})` : loc;
    $('sidebarSizeBadge').textContent = formatSizeLabel(row.plot_size, row.is_apartment);
    $('sidebarEstateTitle').textContent = title;
    $('sidebarEstateSub').textContent = I18n.t('sidebar.owner_line', {
      style: formatStyleName(est.pattern_name, 'game.styles.default'),
      owner: row.owner_name || 'Simulator',
    });
    $('sidebarEstateStats').innerHTML = `
      <span class="badge badge-muted">${esc(I18n.t('sidebar.stat_indoor', { count: est.indoor_count }))}</span>
      <span class="badge badge-muted">${esc(I18n.t('sidebar.stat_outdoor', { count: est.outdoor_count }))}</span>
      <span class="badge badge-muted">${esc(I18n.t('sidebar.stat_snapshots', { count: snapCount }))}</span>
    `;
    btnIds.forEach((id) => { $(id).disabled = $(id).dataset.busy === '1'; });
  }

  function getFilteredSortedEstates() {
    const query = $('estatesSearchInput').value.trim().toLowerCase();
    const dist = $('estatesDistrictFilter').value;
    const size = $('estatesSizeFilter').value;
    const sortBy = $('estatesSortSelect').value;

    const filtered = state.estates.filter((est) => {
      const r = est.row;
      if (state.favoritesOnly && !state.favorites[r.land_ident]) return false;
      if (dist !== 'all' && String(r.territory_type_id) !== dist) return false;
      if (size === 'apartment' && !r.is_apartment) return false;
      if (size !== 'all' && size !== 'apartment') {
        if (r.is_apartment || String(r.plot_size) !== size) return false;
      }
      if (!query) return true;
      const loc = formatEstateLocation(r).toLowerCase();
      const memo = (state.memos[r.land_ident] || '').toLowerCase();
      const hay = [
        r.estate_name,
        r.owner_name,
        est.pattern_name,
        loc,
        memo,
        String(r.land_ident),
      ].join(' ').toLowerCase();
      return hay.includes(query);
    });

    filtered.sort((a, b) => {
      const favDelta = Number(!!state.favorites[b.row.land_ident]) - Number(!!state.favorites[a.row.land_ident]);
      if (favDelta !== 0) return favDelta;

      if (sortBy === 'name') {
        const na = (a.row.estate_name || formatEstateLocation(a.row)).toLowerCase();
        const nb = (b.row.estate_name || formatEstateLocation(b.row)).toLowerCase();
        return na.localeCompare(nb, I18n.locale());
      }
      if (sortBy === 'furniture') {
        return (b.indoor_count + b.outdoor_count) - (a.indoor_count + a.outdoor_count);
      }
      if (sortBy === 'location') {
        return (
          a.row.territory_type_id - b.row.territory_type_id
          || a.row.ward_index - b.row.ward_index
          || Number(a.row.is_apartment) - Number(b.row.is_apartment)
          || (a.row.division * 30 + a.row.plot_index) - (b.row.division * 30 + b.row.plot_index)
          || a.row.room_number - b.row.room_number
        );
      }
      return (b.row.updated_at || 0) - (a.row.updated_at || 0);
    });

    return filtered;
  }

  function renderEstates() {
    const container = $('estatesContainer');
    const items = getFilteredSortedEstates();
    const onlineIdent = state.primarySession?.active_estate?.land_ident ?? null;

    $('btnViewCards').classList.toggle('active', state.viewMode === 'cards');
    $('btnViewTable').classList.toggle('active', state.viewMode === 'table');

    if (items.length === 0) {
      container.className = '';
      container.innerHTML = `<div class="empty-state">${esc(I18n.t('estates.empty'))}</div>`;
      return;
    }

    if (state.viewMode === 'table') {
      container.className = 'estates-table-mode panel';
      container.innerHTML = `
        <div class="table-wrap">
          <table class="data-table">
            <thead>
              <tr>
                <th>★</th>
                <th>${esc(I18n.t('estates.col_type'))}</th>
                <th>${esc(I18n.t('estates.col_name_loc'))}</th>
                <th>${esc(I18n.t('estates.col_style'))}</th>
                <th>${esc(I18n.t('estates.col_owner'))}</th>
                <th>${esc(I18n.t('estates.col_counts'))}</th>
                <th>${esc(I18n.t('estates.col_actions'))}</th>
              </tr>
            </thead>
            <tbody>
              ${items.map((est) => {
                const r = est.row;
                const isSel = r.land_ident === state.selectedLandIdent;
                const isOnline = r.land_ident === onlineIdent;
                const isFav = !!state.favorites[r.land_ident];
                const loc = formatEstateLocation(r);
                const memo = state.memos[r.land_ident];
                return `
                  <tr class="${isSel ? 'selected' : ''}" data-select-estate="${r.land_ident}">
                    <td><button class="fav-btn ${isFav ? 'active' : ''}" data-fav-estate="${r.land_ident}" type="button">${isFav ? '★' : '☆'}</button></td>
                    <td>
                      <span class="badge badge-accent">${esc(formatSizeLabel(r.plot_size, r.is_apartment))}</span>
                      ${isOnline ? `<span class="badge badge-success">${esc(I18n.t('estates.online_badge'))}</span>` : ''}
                    </td>
                    <td>
                      <strong>${esc(r.estate_name || loc)}</strong>
                      <div class="muted small-text">${esc(loc)}${memo ? ` · ${esc(memo)}` : ''}</div>
                    </td>
                    <td>${esc(formatStyleName(est.pattern_name, 'game.styles.default'))}</td>
                    <td>${esc(r.owner_name || 'Simulator')}</td>
                    <td>${est.indoor_count} / ${est.outdoor_count}</td>
                    <td>
                      <div class="button-row">
                        <button class="btn btn-primary btn-xs" data-enter-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_indoor_short'))}</button>
                        <button class="btn btn-secondary btn-xs" data-exit-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_outside_short'))}</button>
                        <button class="btn btn-danger btn-xs" data-delete-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_delete'))}</button>
                      </div>
                    </td>
                  </tr>
                `;
              }).join('')}
            </tbody>
          </table>
        </div>
      `;
      return;
    }

    container.className = 'estates-grid';
    container.innerHTML = items.map((est) => {
      const r = est.row;
      const isSel = r.land_ident === state.selectedLandIdent;
      const isOnline = r.land_ident === onlineIdent;
      const isFav = !!state.favorites[r.land_ident];
      const loc = formatEstateLocation(r);
      const memo = state.memos[r.land_ident];
      return `
        <article class="estate-card ${isSel ? 'selected' : ''} ${isOnline ? 'online' : ''}" data-select-estate="${r.land_ident}">
          <div class="estate-card-top">
            <div class="estate-card-badges">
              <span class="badge badge-accent">${esc(formatSizeLabel(r.plot_size, r.is_apartment))}</span>
              ${isOnline ? `<span class="badge badge-success">${esc(I18n.t('estates.online_badge'))}</span>` : ''}
              <span class="badge badge-muted">${esc(formatStyleName(est.pattern_name, 'game.styles.default'))}</span>
            </div>
            <button class="fav-btn ${isFav ? 'active' : ''}" data-fav-estate="${r.land_ident}" type="button" title="★">${isFav ? '★' : '☆'}</button>
          </div>
          <div>
            <h3 class="estate-title">${esc(r.estate_name || loc)}</h3>
            <div class="estate-meta">${esc(loc)} · ${esc(r.owner_name || 'Simulator')}</div>
          </div>
          ${memo ? `<div class="estate-memo">${esc(memo)}</div>` : ''}
          <div class="estate-card-badges">
            <span class="badge badge-muted">${esc(I18n.t('estates.card_indoor', { count: est.indoor_count }))}</span>
            <span class="badge badge-muted">${esc(I18n.t('estates.card_outdoor', { count: est.outdoor_count }))}</span>
            <span class="badge badge-muted">${esc(I18n.t('estates.card_snapshots', { count: est.snapshot_count }))}</span>
          </div>
          <div class="estate-actions">
            <button class="btn btn-primary btn-xs" data-enter-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_enter'))}</button>
            <button class="btn btn-secondary btn-xs" data-exit-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_outside'))}</button>
            <button class="btn btn-secondary btn-xs" data-reset-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_reset'))}</button>
            <button class="btn btn-danger btn-xs" data-delete-estate="${r.land_ident}" type="button">${esc(I18n.t('estates.btn_delete'))}</button>
          </div>
        </article>
      `;
    }).join('');
  }

  function renderPresets() {
    const targetSel = $('presetTargetSelect');
    targetSel.innerHTML = state.estates.length === 0
      ? `<option value="">${esc(I18n.t('presets.no_estates_option'))}</option>`
      : state.estates.map((e) => {
          const r = e.row;
          const label = `${formatEstateLocation(r)} · ${formatSizeLabel(r.plot_size, r.is_apartment)}${r.estate_name ? ` (${r.estate_name})` : ''}`;
          return `<option value="${r.land_ident}" ${r.land_ident === state.selectedLandIdent ? 'selected' : ''}>${esc(label)}</option>`;
        }).join('');

    const search = $('presetsSearchInput').value.trim().toLowerCase();
    const sizeFilter = $('presetsSizeFilter').value;

    const list = getAllPresets().filter((p) => {
      if (sizeFilter === 'apartment' && !p.isApartment) return false;
      if (sizeFilter !== 'all' && sizeFilter !== 'apartment') {
        if (p.isApartment || String(p.normalizedPlotSize) !== sizeFilter) return false;
      }
      if (!search) return true;
      return `${p.title} ${getPresetStyleText(p)} ${p.districtRaw || ''} ${getPresetSourceLabel(p)}`.toLowerCase().includes(search);
    });

    const listEl = $('presetList');
    if (list.length === 0) {
      listEl.innerHTML = `<div class="empty-state">${esc(I18n.t('presets.list_empty'))}</div>`;
    } else {
      listEl.innerHTML = list.map((p) => {
        const badgeCls = p.sourceKindKey === 'server' ? 'badge-accent' : 'badge-success';
        const countLine = `${I18n.t('sidebar.stat_indoor', { count: p.indoorCount })} · ${I18n.t('sidebar.stat_outdoor', { count: p.outdoorCount })}`;
        return `
          <div class="preset-item ${p.id === state.selectedPresetId ? 'selected' : ''}" data-select-preset="${esc(p.id)}">
            <div class="preset-item-top">
              <div>
                <span class="badge ${badgeCls}">${esc(getPresetSourceLabel(p))}</span>
                <span class="badge badge-muted">${esc(getPresetSizeText(p))}</span>
                <strong>${esc(p.title)}</strong>
              </div>
              <span class="muted small-text">${esc(countLine)}</span>
            </div>
            <div class="muted small-text">${esc(getPresetStyleText(p))}${p.dir ? ` · ${esc(p.dir)}` : ''}</div>
          </div>
        `;
      }).join('');
    }

    const preset = getSelectedPreset();
    const target = getSelectedEstate();
    const compat = evaluateCompat(preset, target);
    const banner = $('presetCompatBanner');
    banner.textContent = compat.text;
    banner.className = `compat-banner ${compat.status === 'match' ? 'match' : compat.status === 'warn' ? 'warn' : ''}`;
    $('btnPresetAutoMatch').classList.toggle('hidden', !compat.mismatch);
    $('btnRevealPresetFile')?.classList.toggle('hidden', !preset?.diskPath);

    if (!preset) {
      $('presetDetailTitle').textContent = I18n.t('presets.detail_none_title');
      $('presetDetailBadge').textContent = I18n.t('presets.detail_none_badge');
      $('presetDetailCounts').textContent = I18n.t('presets.detail_none_counts');
      $('presetDetailFixtures').innerHTML = '';
    } else if (preset.parseError) {
      $('presetDetailTitle').textContent = preset.title;
      $('presetDetailBadge').textContent = I18n.t('presets.detail_parse_error');
      $('presetDetailCounts').textContent = preset.parseError;
      $('presetDetailFixtures').innerHTML = '';
    } else {
      $('presetDetailTitle').textContent = preset.title;
      $('presetDetailBadge').textContent = `${getPresetSizeText(preset)} · ${getPresetStyleText(preset)}`;
      const details = [];
      if (preset.indoorDyed > 0) details.push(I18n.t('presets.detail_dyed', { count: preset.indoorDyed }));
      if (preset.indoorMaterial > 0) details.push(I18n.t('presets.detail_material', { count: preset.indoorMaterial }));
      const inExtra = details.length ? ` (${details.join(', ')})` : '';
      const outExtra = preset.outdoorDyed > 0
        ? ` (${I18n.t('presets.detail_dyed', { count: preset.outdoorDyed })})`
        : '';
      $('presetDetailCounts').textContent = I18n.t('presets.detail_counts', {
        indoor: preset.indoorCount,
        inExtra,
        outdoor: preset.outdoorCount,
        outExtra,
      });
      const inList = (preset.interiorFixtures || []).map((f) => I18n.formatFixture(f)).filter(Boolean);
      const outList = (preset.exteriorFixtures || []).map((f) => I18n.formatFixture(f)).filter(Boolean);
      const inFix = inList.length ? inList.join(' / ') : I18n.t('presets.detail_none');
      const outFix = outList.length ? outList.join(' / ') : I18n.t('presets.detail_none');
      $('presetDetailFixtures').innerHTML = `
        <div><strong>${esc(I18n.t('presets.detail_int_fixtures'))}</strong> ${esc(inFix)}</div>
        <div><strong>${esc(I18n.t('presets.detail_ext_fixtures'))}</strong> ${esc(outFix)}</div>
      `;
    }
  }

  function getActiveFurnitureList() {
    if (!state.bridgeSnapshot || state.bridgeSnapshot.land_ident !== state.selectedLandIdent) return [];
    return state.furnScope === 'indoor' ? (state.bridgeSnapshot.indoor || []) : (state.bridgeSnapshot.outdoor || []);
  }

  function syncSelectedFurnitureIntoDraft({ force = true } = {}) {
    const list = getActiveFurnitureList();
    const item = list.find((f) => f.flat_slot === state.selectedFurnSlot) || null;
    if (!item) {
      state.selectedFurnSlot = null;
      state.transformDraft = null;
      state.transformOriginal = null;
      state.transformDirty = false;
      return;
    }
    const usePlotLocal = state.furnScope === 'outdoor' && $('tfPlotLocal').checked;
    const coords = {
      x: usePlotLocal ? item.local_x : item.pos_x,
      y: usePlotLocal ? item.local_y : item.pos_y,
      z: usePlotLocal ? item.local_z : item.pos_z,
      rotDeg: wrapDegrees(((usePlotLocal ? item.local_rotation : item.rotation) * 180) / Math.PI),
    };
    state.transformOriginal = { ...coords };
    if (force || !state.transformDraft || !state.transformDirty) {
      state.transformDraft = { ...coords };
      state.transformDirty = false;
    }
  }

  function renderFurniture() {
    const snap = state.bridgeSnapshot?.land_ident === state.selectedLandIdent ? state.bridgeSnapshot : null;
    const indoorList = snap?.indoor || [];
    const outdoorList = snap?.outdoor || [];
    $('furnIndoorCount').textContent = indoorList.length;
    $('furnOutdoorCount').textContent = outdoorList.length;
    $('btnFurnScopeIndoor').classList.toggle('active', state.furnScope === 'indoor');
    $('btnFurnScopeOutdoor').classList.toggle('active', state.furnScope === 'outdoor');
    $('tfPlotLocalWrap').classList.toggle('hidden', state.furnScope !== 'outdoor');

    const search = $('furnSearchInput').value.trim().toLowerCase();
    const dyedOnly = $('furnDyedOnly').checked;
    const sortBy = $('furnSortSelect').value;

    const list = getActiveFurnitureList().filter((item) => {
      if (dyedOnly && !(item.stain > 0 || item.color_hex)) return false;
      if (!search) return true;
      return `${item.name} ${item.item_id} ${item.flat_slot}`.toLowerCase().includes(search);
    });

    list.sort((a, b) => {
      if (sortBy === 'name') return a.name.localeCompare(b.name, I18n.locale());
      if (sortBy === 'y') return b.pos_y - a.pos_y;
      return a.flat_slot - b.flat_slot;
    });

    const tbody = $('furnitureTableBody');
    if (!state.selectedLandIdent) {
      tbody.innerHTML = `<tr><td colspan="8" class="muted">${esc(I18n.t('furniture.select_estate_first'))}</td></tr>`;
    } else if (list.length === 0) {
      tbody.innerHTML = `<tr><td colspan="8" class="muted">${esc(I18n.t('furniture.empty'))}</td></tr>`;
    } else {
      const usePlotLocal = state.furnScope === 'outdoor' && $('tfPlotLocal').checked;
      tbody.innerHTML = list.map((item) => {
        const x = usePlotLocal ? item.local_x : item.pos_x;
        const y = usePlotLocal ? item.local_y : item.pos_y;
        const z = usePlotLocal ? item.local_z : item.pos_z;
        const deg = wrapDegrees(((usePlotLocal ? item.local_rotation : item.rotation) * 180) / Math.PI);
        const cleanHex = item.color_hex ? String(item.color_hex).replace(/^#/, '').slice(0, 6) : '';
        const swatch = /^[0-9a-fA-F]{6}$/.test(cleanHex)
          ? `<span class="swatch" style="background:#${cleanHex}"></span>`
          : '';
        return `
          <tr class="${item.flat_slot === state.selectedFurnSlot ? 'selected' : ''}" data-furn-slot="${item.flat_slot}">
            <td>${item.flat_slot}</td>
            <td><strong>${esc(item.name)}</strong></td>
            <td>#${item.item_id}</td>
            <td>${swatch}${item.stain > 0 ? item.stain : '-'}</td>
            <td>${x.toFixed(2)}</td>
            <td>${y.toFixed(2)}</td>
            <td>${z.toFixed(2)}</td>
            <td>${deg.toFixed(1)}°</td>
          </tr>
        `;
      }).join('');
    }

    const activeItem = getActiveFurnitureList().find((f) => f.flat_slot === state.selectedFurnSlot) || null;
    if (!activeItem || !state.transformDraft) {
      $('tfItemName').textContent = I18n.t('furniture.none_selected');
      $('tfItemMeta').textContent = I18n.t('furniture.none_meta');
      $('tfSlotBadge').textContent = '-';
      $('tfPosX').value = '';
      $('tfPosY').value = '';
      $('tfPosZ').value = '';
      $('tfRotDeg').value = '';
      $('tfRotSlider').value = 0;
      $('btnTfRevert').disabled = true;
      $('btnTfCommit').disabled = true;
      return;
    }

    $('tfItemName').textContent = activeItem.name;
    $('tfItemMeta').textContent = `Item #${activeItem.item_id} · Catalog #${activeItem.catalog_id}`;
    const scopeLabel = I18n.t(state.furnScope === 'indoor' ? 'furniture.indoor_tab' : 'furniture.outdoor_tab');
    $('tfSlotBadge').textContent = `${scopeLabel} #${activeItem.flat_slot}`;
    if (document.activeElement !== $('tfPosX')) $('tfPosX').value = state.transformDraft.x.toFixed(3);
    if (document.activeElement !== $('tfPosY')) $('tfPosY').value = state.transformDraft.y.toFixed(3);
    if (document.activeElement !== $('tfPosZ')) $('tfPosZ').value = state.transformDraft.z.toFixed(3);
    if (document.activeElement !== $('tfRotDeg')) $('tfRotDeg').value = state.transformDraft.rotDeg.toFixed(1);
    if (document.activeElement !== $('tfRotSlider')) {
      $('tfRotSlider').value = Math.max(-180, Math.min(180, state.transformDraft.rotDeg));
    }
    $('btnTfRevert').disabled = false;
    $('btnTfCommit').disabled = $('btnTfCommit').dataset.busy === '1';
  }

  const ENV_ACTION_BUTTON_IDS = [
    'btnEnvSaveLight',
    'btnEnvApplyStyle',
    'btnEnvSaveMeta',
    'btnEnvSaveMemo',
    'btnResetIndoor',
    'btnResetExterior',
    'btnResetAll',
    'btnEnvForceReload',
    'btnDeleteEstate',
    'btnSnapshotCreate',
    'btnSnapshotUndo',
  ];

  function syncEnvironmentButtonStates(estSummary) {
    if (!estSummary) {
      $('envStyleSelect').disabled = true;
      ENV_ACTION_BUTTON_IDS.forEach((id) => {
        const el = $(id);
        if (el) el.disabled = true;
      });
      return;
    }
    const isApt = !!estSummary.row.is_apartment;
    $('envStyleSelect').disabled = isApt;
    ENV_ACTION_BUTTON_IDS.forEach((id) => {
      const el = $(id);
      if (!el) return;
      if (id === 'btnEnvApplyStyle') {
        el.disabled = isApt || el.dataset.busy === '1';
      } else {
        el.disabled = el.dataset.busy === '1';
      }
    });
  }

  function renderEnvironment({ forceSync = false } = {}) {
    const estSummary = getSelectedEstate();
    if (!estSummary) {
      document.querySelectorAll('#envLightPills [data-light]').forEach((btn) => {
        btn.classList.remove('active');
      });
      $('envSsaoCheck').checked = false;
      $('envVisitorsCheck').checked = false;
      $('envNameInput').value = '';
      $('envGreetingInput').value = '';
      $('envMemoInput').value = '';
      $('envNameBytes').textContent = '0';
      $('envGreetingBytes').textContent = '0';
      $('snapshotTimeline').innerHTML = `<div class="empty-state">${esc(I18n.t('env.snap_empty_select'))}</div>`;
      $('envFixturesContent').innerHTML = '';
      syncEnvironmentButtonStates(null);
      return;
    }

    syncEnvironmentButtonStates(estSummary);
    const detail = state.selectedEstateDetail?.estate?.land_ident === estSummary.row.land_ident
      ? state.selectedEstateDetail
      : null;
    const row = detail?.estate || estSummary.row;

    if (forceSync || !state.envDirty.light) {
      document.querySelectorAll('#envLightPills [data-light]').forEach((btn) => {
        btn.classList.toggle('active', Number(btn.dataset.light) === row.light_level);
      });
      $('envSsaoCheck').checked = !!row.enable_ssao;
      $('envVisitorsCheck').checked = (row.flags & 0x01) !== 0;
    }

    if (forceSync || (!state.envDirty.style && document.activeElement !== $('envStyleSelect'))) {
      $('envStyleSelect').value = normalizeStyleKey(estSummary.pattern_name);
    }

    if (
      forceSync
      || (!state.envDirty.meta
        && document.activeElement !== $('envNameInput')
        && document.activeElement !== $('envGreetingInput'))
    ) {
      $('envNameInput').value = row.estate_name || '';
      $('envGreetingInput').value = row.greeting || '';
    }
    if (forceSync || (!state.envDirty.memo && document.activeElement !== $('envMemoInput'))) {
      $('envMemoInput').value = state.memos[row.land_ident] || '';
    }
    $('envNameBytes').textContent = utf8ByteLength($('envNameInput').value);
    $('envGreetingBytes').textContent = utf8ByteLength($('envGreetingInput').value);

    const ext = detail?.exterior;
    const int = detail?.interior;
    if (ext && int) {
      const wStain = esc(I18n.t('env.fixtures_stain', { stain: int.window_stain }));
      const dStain = esc(I18n.t('env.fixtures_stain', { stain: int.door_stain }));
      $('envFixturesContent').innerHTML = `
        <div><strong>${esc(I18n.t('env.fixtures_detail_rows'))}:</strong> ${int.renovation_row_id}</div>
        <div><strong>${esc(I18n.t('env.fixtures_win_door'))}:</strong> ${int.window_style} (${wStain}) / ${int.door_style} (${dStain})</div>
        <div><strong>${esc(I18n.t('env.fixtures_f1'))}:</strong> ${int.ground_walls} / ${int.ground_floor} / ${int.ground_chandelier}</div>
        <div><strong>${esc(I18n.t('env.fixtures_f2'))}:</strong> ${int.top_walls} / ${int.top_floor} / ${int.top_chandelier}</div>
        <div><strong>${esc(I18n.t('env.fixtures_f3'))}:</strong> ${int.cellar_walls} / ${int.cellar_floor} / ${int.cellar_chandelier}</div>
        <div><strong>${esc(I18n.t('env.fixtures_ext'))}:</strong> ${ext.roof_id}/${ext.walls_id}/${ext.windows_id}/${ext.door_id}/${ext.fence_id}</div>
      `;
    } else {
      $('envFixturesContent').innerHTML = `<div class="muted">${esc(I18n.t('env.fixtures_loading'))}</div>`;
    }

    const snaps = detail?.snapshots || [];
    if (snaps.length === 0) {
      $('snapshotTimeline').innerHTML = `<div class="empty-state">${esc(I18n.t('env.snap_empty'))}</div>`;
    } else {
      $('snapshotTimeline').innerHTML = snaps.map((s) => `
        <div class="snapshot-item">
          <div>
            <strong>#${s.id} · ${esc(s.label)}</strong>
            <div class="muted small-text">${esc(formatTimestamp(s.created_at))}</div>
          </div>
          <button class="btn btn-secondary btn-sm" data-restore-snapshot="${s.id}" type="button">${esc(I18n.t('env.btn_restore'))}</button>
        </div>
      `).join('');
    }
  }

  function renderLogs() {
    const q = $('logsSearchInput').value.trim().toLowerCase();
    const filtered = state.logs.filter((entry) => {
      if (state.logCategory !== 'all' && entry.category !== state.logCategory) return false;
      if (!q) return true;
      return `${entry.kind} ${entry.source} ${entry.message} ${entry.landIdent ?? ''}`.toLowerCase().includes(q);
    });

    $('navCountLogs').textContent = state.logs.length;
    const feed = $('logFeed');
    if (filtered.length === 0) {
      feed.innerHTML = `<div class="empty-state">${esc(I18n.t('logs.empty'))}</div>`;
      return;
    }

    feed.innerHTML = filtered.map((l) => `
      <div class="log-row">
        <span class="log-time">[${esc(l.time)}]</span>
        <span class="badge badge-muted">${esc(l.kind)}</span>
        ${l.source ? `<span class="badge badge-accent">${esc(l.source)}</span>` : ''}
        ${l.landIdent !== null ? `<span class="badge badge-muted">#${l.landIdent}</span>` : ''}
        <span>${esc(l.message)}</span>
      </div>
    `).join('');
  }

  // ---- Actions ----

  function buildCreateOrSwitchPayload(enterInterior, forEnsure = false) {
    const district = $('createDistrict').value;
    const divisionRaw = Math.trunc(Number($('createDivision').value)) || 0;
    const sizeVal = $('createSize').value;
    const isApartment = sizeVal === 'apartment';
    const plotSize = isApartment ? 0 : Math.trunc(Number(sizeVal));
    const wardHuman = Math.max(1, Math.min(30, Math.trunc(Number($('createWard').value)) || 1));
    const plotOrRoomRaw = Math.trunc(Number($('createPlotOrRoom').value)) || 0;

    let division = divisionRaw;
    let plotIndex = null;
    let roomNumber = isApartment ? 1 : 0;

    if (plotOrRoomRaw > 0) {
      if (isApartment) {
        roomNumber = Math.max(1, Math.min(90, plotOrRoomRaw));
      } else {
        const zeroBased = Math.max(0, Math.min(59, plotOrRoomRaw - 1));
        division = zeroBased >= 30 ? 1 : 0;
        plotIndex = zeroBased % 30;
      }
    }

    const estateName = $('createEstateName').value.trim() || null;
    const ownerName = $('createOwnerName').value.trim() || null;

    return {
      district,
      ward_index: wardHuman - 1,
      division,
      plot_index: plotIndex,
      room_number: isApartment ? roomNumber : 0,
      is_apartment: isApartment,
      plot_size: (isApartment || plotIndex !== null) ? null : plotSize,
      estate_name: estateName,
      owner_name: forEnsure
        ? (ownerName || state.primarySession?.character_name || 'Simulator')
        : (ownerName || state.primarySession?.character_name || null),
      owner_content_id: forEnsure
        ? (state.primarySession?.content_id ?? 1)
        : (state.primarySession?.content_id ?? null),
      enter_interior: enterInterior,
    };
  }

  async function ensureOnlyEstate() {
    try {
      const payload = buildCreateOrSwitchPayload(true, true);
      const row = await apiJson('/estates/ensure', {
        method: 'POST',
        body: JSON.stringify(payload),
      });
      toast(I18n.t('estates.ensure_ok', { loc: formatEstateLocation(row) }));
      $('createEstatePanel').classList.add('hidden');
      await refreshAll(row.land_ident, { forceSync: true });
    } catch (err) {
      toast(I18n.t('estates.ensure_fail', { error: err.message }), true);
    }
  }

  async function switchWithCreateForm(enterInterior) {
    try {
      const payload = buildCreateOrSwitchPayload(enterInterior, false);
      const res = await apiJson('/session/switch', {
        method: 'POST',
        body: JSON.stringify(payload),
      });
      const loc = formatEstateLocation(res.estate);
      if (res.online_sessions_notified === 0) {
        toast(I18n.t('estates.switch_offline_ok', { loc }));
      } else {
        toast(I18n.t(enterInterior ? 'estates.switch_enter_ok' : 'estates.switch_outside_ok', { loc }));
      }
      $('createEstatePanel').classList.add('hidden');
      await refreshAll(res.estate.land_ident, { forceSync: true });
    } catch (err) {
      toast(I18n.t('estates.switch_fail', { error: err.message }), true);
    }
  }

  async function warpToEstate(landIdent, enterInterior) {
    try {
      const res = await apiJson('/session/switch', {
        method: 'POST',
        body: JSON.stringify({ land_ident: landIdent, enter_interior: enterInterior }),
      });
      const loc = formatEstateLocation(res.estate);
      if (res.online_sessions_notified === 0) {
        toast(I18n.t('estates.switch_offline_ok', { loc }));
      } else {
        toast(I18n.t(enterInterior ? 'estates.switch_enter_ok' : 'estates.switch_outside_ok', { loc }));
      }
      await refreshAll(res.estate.land_ident, { forceSync: true });
    } catch (err) {
      toast(I18n.t('estates.switch_fail', { error: err.message }), true);
    }
  }

  async function reloadSelectedEstate(btn) {
    if (!state.selectedLandIdent) return;
    await runBusy(btn, async () => {
      try {
        await apiJson(`/estates/${state.selectedLandIdent}/reload`, {
          method: 'POST',
          body: JSON.stringify({ mode: 'zone_reload', preserve_pos: true, force_enter: true }),
        });
        toast(I18n.t('estates.reload_ok'));
      } catch (err) {
        toast(I18n.t('estates.reload_fail', { error: err.message }), true);
      }
    });
  }

  async function undoSelectedEstate(btn) {
    if (!state.selectedLandIdent) return;
    await runBusy(btn, async () => {
      try {
        const res = await apiJson(`/estates/${state.selectedLandIdent}/undo`, {
          method: 'POST',
          body: JSON.stringify({ reload_mode: 'auto', preserve_pos: true, source: 'web_ui' }),
        });
        toast(I18n.t('estates.undo_ok', { id: res.id }));
        await refreshAll(state.selectedLandIdent, { forceSync: true });
      } catch (err) {
        toast(I18n.t('estates.undo_fail', { error: err.message }), true);
      }
    });
  }

  async function resetEstateScope(landIdent, scope) {
    const scopeKey = scope === 'interior'
      ? 'estates.scope_interior'
      : scope === 'exterior'
        ? 'estates.scope_exterior'
        : 'estates.scope_all';
    const label = I18n.t(scopeKey);
    const ok = await askConfirm(
      I18n.t('estates.reset_confirm_title'),
      I18n.t('estates.reset_confirm_msg', { scope: label })
    );
    if (!ok) return;

    try {
      const res = await apiJson(`/estates/${landIdent}/reset`, {
        method: 'POST',
        body: JSON.stringify({ scope, reload_mode: 'auto', source: 'web_ui' }),
      });
      toast(I18n.t('estates.reset_ok', { scope: label, count: res.removed_count }));
      await refreshAll(landIdent, { forceSync: true });
    } catch (err) {
      toast(I18n.t('estates.reset_fail', { error: err.message }), true);
    }
  }

  async function deleteEstate(landIdent) {
    const ok = await askConfirm(
      I18n.t('estates.delete_confirm_title'),
      I18n.t('estates.delete_confirm_msg', { id: landIdent })
    );
    if (!ok) return;

    try {
      await apiJson(`/estates/${landIdent}`, { method: 'DELETE' });
      toast(I18n.t('estates.delete_ok', { id: landIdent }));
      if (state.selectedLandIdent === landIdent) {
        state.selectedLandIdent = null;
        clearSelectionTransientState();
      }
      await refreshAll(null, { forceSync: true });
    } catch (err) {
      toast(I18n.t('estates.delete_fail', { error: err.message }), true);
    }
  }

  async function handlePickedPresetFiles(fileList) {
    const files = Array.from(fileList || []).filter((f) => f.name.toLowerCase().endsWith('.json'));
    if (files.length === 0) {
      toast(I18n.t('presets.no_json_picked'), true);
      return;
    }
    const parsed = await Promise.all(files.map(async (f) => {
      const buf = await f.arrayBuffer();
      const text = decodeBufferWithBom(buf);
      return parseLocalPresetFile(f.name, text, f.lastModified, f.webkitRelativePath || '');
    }));

    for (const item of parsed) {
      const idx = state.localPresets.findIndex((p) => p.id === item.id);
      if (idx >= 0) state.localPresets[idx] = item;
      else state.localPresets.unshift(item);
    }
    state.selectedPresetId = parsed[0].id;
    renderPresets();
    renderSidebarSelected();
    toast(I18n.t('presets.loaded_local_files', { count: parsed.length }));
  }

  function buildPresetRequestBody(preset) {
    const scope = $('presetScopeSelect').value;
    const reloadMode = $('presetReloadSelect').value;
    const preservePos = $('presetPreservePos').checked;
    return {
      ...(preset.rawJson !== null ? { raw_json: preset.rawJson } : { path: preset.serverPath }),
      scope,
      reload_mode: reloadMode,
      preserve_pos: preservePos,
      label: preset.title,
      source: 'web_ui',
    };
  }

  async function dispatchLiveMove({ commitDb }) {
    if (!state.selectedLandIdent || state.selectedFurnSlot === null || !state.transformDraft) return;
    const indoors = state.furnScope === 'indoor';
    const plotLocal = !indoors && $('tfPlotLocal').checked;
    const item = getActiveFurnitureList().find((f) => f.flat_slot === state.selectedFurnSlot);
    const d = state.transformDraft;
    const payload = {
      source: 'web_ui',
      indoors,
      plot_local: plotLocal,
      commit_db: commitDb,
      moves: [{
        flat_slot: state.selectedFurnSlot,
        item_id: item?.item_id ?? null,
        pos_x: Number(d.x),
        pos_y: Number(d.y),
        pos_z: Number(d.z),
        rotation: (wrapDegrees(Number(d.rotDeg)) * Math.PI) / 180,
      }],
    };
    const res = await apiJson(`/estates/${state.selectedLandIdent}/live/move`, {
      method: 'POST',
      body: JSON.stringify(payload),
    });
    if (commitDb) {
      state.transformDirty = false;
      toast(I18n.t('furniture.commit_ok', { rev: res.revision }));
      await refreshBridgeSnapshot({ forceSyncDraft: true });
    }
  }

  function scheduleLivePreview() {
    if (!$('tfAutoPreview').checked) return;
    cancelPendingLiveMove();
    state.liveMoveTimer = setTimeout(() => {
      state.liveMoveTimer = null;
      dispatchLiveMove({ commitDb: false }).catch((err) =>
        toast(I18n.t('furniture.preview_err', { error: err.message }), true)
      );
    }, 80);
  }

  // ---- Event Wiring ----

  function bindEvents() {
    document.querySelectorAll('.nav-item').forEach((btn) => {
      btn.addEventListener('click', () => switchTab(btn.dataset.tab));
    });

    $('btnRefreshAll').addEventListener('click', () => refreshAll(state.selectedLandIdent, { forceSync: true }));

    $('activeSessionPill').addEventListener('click', async () => {
      const ident = state.primarySession?.active_estate?.land_ident;
      if (!ident) {
        toast(I18n.t('estates.no_active_in_game'), true);
        return;
      }
      if (!state.estates.some((e) => e.row.land_ident === ident)) {
        toast(I18n.t('estates.active_not_in_list'));
        await refreshAll();
        return;
      }
      selectEstate(ident);
      toast(I18n.t('estates.selected_active'));
    });

    $('btnUnlockAll').addEventListener('click', () => runBusy($('btnUnlockAll'), async () => {
      try {
        const res = await apiJson('/session/unlock-all', { method: 'POST', body: '{}' });
        toast(I18n.t('topbar.unlock_done', { count: res.unlocked_characters }));
      } catch (err) {
        toast(I18n.t('topbar.unlock_fail', { error: err.message }), true);
      }
    }));

    document.querySelectorAll('[data-give-kit]').forEach((btn) => {
      btn.addEventListener('click', () => runBusy(btn, async () => {
        try {
          const kind = btn.dataset.giveKit;
          const res = await apiJson('/session/give-kit', {
            method: 'POST',
            body: JSON.stringify({ kind }),
          });
          toast(
            I18n.t('topbar.kit_done', {
              kind: I18n.t(`topbar.kit_${kind}`),
              count: res.online_sessions_notified,
            })
          );
        } catch (err) {
          toast(I18n.t('topbar.kit_fail', { error: err.message }), true);
        }
      }));
    });

    // Sidebar pinned quick buttons
    $('sidebarBtnEnter').addEventListener('click', () => {
      if (state.selectedLandIdent) runBusy($('sidebarBtnEnter'), () => warpToEstate(state.selectedLandIdent, true));
    });
    $('sidebarBtnExit').addEventListener('click', () => {
      if (state.selectedLandIdent) runBusy($('sidebarBtnExit'), () => warpToEstate(state.selectedLandIdent, false));
    });
    $('sidebarBtnReload').addEventListener('click', () => reloadSelectedEstate($('sidebarBtnReload')));
    $('sidebarBtnUndo').addEventListener('click', () => undoSelectedEstate($('sidebarBtnUndo')));

    // Estates view
    $('btnToggleCreateEstate').addEventListener('click', () => $('createEstatePanel').classList.toggle('hidden'));
    $('btnCloseCreateEstate').addEventListener('click', () => $('createEstatePanel').classList.add('hidden'));
    $('btnCreateEnsureOnly').addEventListener('click', () => runBusy($('btnCreateEnsureOnly'), ensureOnlyEstate));
    $('btnCreateAndEnter').addEventListener('click', () => runBusy($('btnCreateAndEnter'), () => switchWithCreateForm(true)));
    $('btnCreateAndOutside').addEventListener('click', () => runBusy($('btnCreateAndOutside'), () => switchWithCreateForm(false)));

    ['estatesSearchInput', 'estatesDistrictFilter', 'estatesSizeFilter', 'estatesSortSelect'].forEach((id) => {
      $(id).addEventListener('input', renderEstates);
      $(id).addEventListener('change', renderEstates);
    });
    $('btnEstatesFavOnly').addEventListener('click', () => {
      state.favoritesOnly = !state.favoritesOnly;
      const favBtn = $('btnEstatesFavOnly');
      favBtn.classList.toggle('btn-primary', state.favoritesOnly);
      favBtn.classList.toggle('btn-secondary', !state.favoritesOnly);
      favBtn.setAttribute('aria-pressed', String(state.favoritesOnly));
      renderEstates();
    });
    $('btnViewCards').addEventListener('click', () => { state.viewMode = 'cards'; savePrefs(); renderEstates(); });
    $('btnViewTable').addEventListener('click', () => { state.viewMode = 'table'; savePrefs(); renderEstates(); });

    $('estatesContainer').addEventListener('click', (ev) => {
      const favBtn = ev.target.closest('[data-fav-estate]');
      if (favBtn) {
        ev.stopPropagation();
        const id = Number(favBtn.dataset.favEstate);
        state.favorites[id] = !state.favorites[id];
        if (!state.favorites[id]) delete state.favorites[id];
        savePrefs();
        renderEstates();
        return;
      }
      const enterBtn = ev.target.closest('[data-enter-estate]');
      if (enterBtn) {
        ev.stopPropagation();
        runBusy(enterBtn, () => warpToEstate(Number(enterBtn.dataset.enterEstate), true));
        return;
      }
      const exitBtn = ev.target.closest('[data-exit-estate]');
      if (exitBtn) {
        ev.stopPropagation();
        runBusy(exitBtn, () => warpToEstate(Number(exitBtn.dataset.exitEstate), false));
        return;
      }
      const resetBtn = ev.target.closest('[data-reset-estate]');
      if (resetBtn) {
        ev.stopPropagation();
        runBusy(resetBtn, () => resetEstateScope(Number(resetBtn.dataset.resetEstate), 'all'));
        return;
      }
      const delBtn = ev.target.closest('[data-delete-estate]');
      if (delBtn) {
        ev.stopPropagation();
        runBusy(delBtn, () => deleteEstate(Number(delBtn.dataset.deleteEstate)));
        return;
      }

      const rowOrCard = ev.target.closest('[data-select-estate]');
      if (rowOrCard) {
        selectEstate(Number(rowOrCard.dataset.selectEstate));
      }
    });

    // Presets view
    $('presetTargetSelect').addEventListener('change', (ev) => {
      const val = Number(ev.target.value);
      if (val) selectEstate(val);
    });
    $('btnPresetSyncActive').addEventListener('click', async () => {
      const ident = state.primarySession?.active_estate?.land_ident;
      if (!ident) {
        toast(I18n.t('estates.no_active_in_game'), true);
        return;
      }
      if (!state.estates.some((e) => e.row.land_ident === ident)) {
        toast(I18n.t('estates.active_not_in_list'));
        await refreshAll();
        return;
      }
      selectEstate(ident);
    });
    $('btnPresetAutoMatch').addEventListener('click', () => {
      const preset = getSelectedPreset();
      if (!preset || preset.normalizedPlotSize === null) return;
      const match = state.estates.find((e) =>
        e.row.is_apartment === preset.isApartment && (preset.isApartment || e.row.plot_size === preset.normalizedPlotSize)
      );
      if (!match) {
        toast(I18n.t('presets.no_matching_estate', { size: getPresetSizeText(preset) }), true);
        return;
      }
      selectEstate(match.row.land_ident);
    });

    const addWatchedFolderFlow = async () => {
      const picked = await Bridge.pickFolder(I18n.t('dialog.pick_watch_dir'), state.watchedDirs[0] || null);
      if (!picked) return;
      try {
        state.watchedDirs = await Bridge.addWatchedPresetDir(picked);
        renderWatchedDirs();
        await refreshLocalPresets();
        toast(I18n.t('presets.watch_added', { dir: picked }));
      } catch (err) {
        toast(I18n.t('presets.watch_add_fail', { error: err.message }), true);
      }
    };

    $('btnAddWatchedDir')?.addEventListener('click', addWatchedFolderFlow);
    $('btnPickPresetFolder').addEventListener('click', addWatchedFolderFlow);
    $('presetAutoApplyCheck')?.addEventListener('change', async (ev) => {
      state.autoApplyWatched = !!ev.target.checked;
      try {
        const s = await Bridge.getSettings();
        s.autoApplyWatchedPresets = state.autoApplyWatched;
        await Bridge.saveSettings(s);
      } catch {
        // Non-fatal; preference stays active for current session.
      }
    });
    $('watchedDirsList')?.addEventListener('click', async (ev) => {
      const removeBtn = ev.target.closest('[data-remove-watch]');
      if (removeBtn) {
        try {
          state.watchedDirs = await Bridge.removeWatchedPresetDir(removeBtn.dataset.removeWatch);
          renderWatchedDirs();
          await refreshLocalPresets();
          toast(I18n.t('presets.watch_removed'));
        } catch (err) {
          toast(I18n.t('presets.watch_remove_fail', { error: err.message }), true);
        }
        return;
      }
      const openBtn = ev.target.closest('[data-open-watch]');
      if (openBtn) {
        Bridge.revealInExplorer(openBtn.dataset.openWatch).catch((err) => toast(err.message, true));
      }
    });

    $('btnPickPresetFiles').addEventListener('click', () => $('presetFilesInput').click());
    $('presetFilesInput').addEventListener('change', (ev) => { handlePickedPresetFiles(ev.target.files); ev.target.value = ''; });
    $('presetFolderInput').addEventListener('change', (ev) => { handlePickedPresetFiles(ev.target.files); ev.target.value = ''; });
    $('btnRefreshServerPresets').addEventListener('click', async () => {
      await refreshLocalPresets();
      await refreshAll(state.selectedLandIdent);
    });

    const dz = $('presetDropZone');
    dz.addEventListener('dragover', (ev) => { ev.preventDefault(); dz.classList.add('dragover'); });
    dz.addEventListener('dragleave', () => dz.classList.remove('dragover'));
    dz.addEventListener('drop', (ev) => {
      ev.preventDefault();
      dz.classList.remove('dragover');
      if (ev.dataTransfer?.files?.length) {
        handlePickedPresetFiles(ev.dataTransfer.files);
      }
    });

    ['presetsSearchInput', 'presetsSizeFilter'].forEach((id) => {
      $(id).addEventListener('input', renderPresets);
      $(id).addEventListener('change', renderPresets);
    });

    $('presetList').addEventListener('click', (ev) => {
      const item = ev.target.closest('[data-select-preset]');
      if (!item) return;
      state.selectedPresetId = item.dataset.selectPreset;
      $('presetPreviewReport').classList.add('hidden');
      renderPresets();
    });

    $('btnRevealPresetFile')?.addEventListener('click', () => {
      const preset = getSelectedPreset();
      if (!preset?.diskPath) return;
      Bridge.revealInExplorer(preset.diskPath).catch((err) =>
        toast(I18n.t('presets.reveal_fail', { error: err.message }), true)
      );
    });

    $('btnPreviewPreset').addEventListener('click', () => runBusy($('btnPreviewPreset'), async () => {
      const preset = getSelectedPreset();
      const target = getSelectedEstate();
      if (!preset || !target) { toast(I18n.t('presets.need_target_and_preset'), true); return; }
      try {
        const report = await previewPresetObject(preset, target);
        const box = $('presetPreviewReport');
        box.classList.remove('hidden');
        const sizeDisplay = report.house_size || I18n.t('game.sizes.unknown');
        const lines = [
          I18n.t('presets.preview_lines_size', { size: sizeDisplay }),
          I18n.t('presets.preview_lines_in', {
            resolved: report.interior_resolved_count,
            input: report.interior_input_count,
            trunc: report.interior_truncated_count,
            fix: report.interior_fixtures_matched,
          }),
          I18n.t('presets.preview_lines_out', {
            resolved: report.exterior_resolved_count,
            input: report.exterior_input_count,
            trunc: report.exterior_truncated_count,
            fix: report.exterior_fixtures_matched,
          }),
        ];
        if (report.warnings?.length) {
          lines.push(`${I18n.t('presets.preview_warnings')}\n- ${report.warnings.join('\n- ')}`);
        }
        if (report.interior_unresolved_items?.length) {
          lines.push(
            I18n.t('presets.preview_unmatched_in', {
              items: report.interior_unresolved_items.slice(0, 8).join(', '),
            })
          );
        }
        if (report.exterior_unresolved_items?.length) {
          lines.push(
            I18n.t('presets.preview_unmatched_out', {
              items: report.exterior_unresolved_items.slice(0, 8).join(', '),
            })
          );
        }
        box.textContent = lines.join('\n');
      } catch (err) {
        toast(I18n.t('presets.preview_fail', { error: err.message }), true);
      }
    }));

    $('btnApplyPreset').addEventListener('click', () => runBusy($('btnApplyPreset'), async () => {
      const preset = getSelectedPreset();
      const target = getSelectedEstate();
      if (!preset || !target) { toast(I18n.t('presets.need_target_and_preset'), true); return; }
      const compat = evaluateCompat(preset, target);
      if (compat.mismatch) {
        const proceed = await askConfirm(
          I18n.t('presets.apply_confirm_title'),
          I18n.t('presets.apply_confirm_msg', { compat: compat.text })
        );
        if (!proceed) return;
      }
      try {
        const res = await applyPresetObject(preset, target);
        toast(
          I18n.t('presets.apply_ok', {
            indoor: res.preview.interior_resolved_count,
            outdoor: res.preview.exterior_resolved_count,
          })
        );
        await refreshAll(target.row.land_ident, { forceSync: true });
      } catch (err) {
        toast(I18n.t('presets.apply_fail', { error: err.message }), true);
      }
    }));

    $('btnExportDownload').addEventListener('click', () => runBusy($('btnExportDownload'), async () => {
      if (!state.selectedLandIdent) { toast(I18n.t('presets.need_estate_first'), true); return; }
      try {
        const scope = $('exportScopeSelect').value;
        const rawName = ($('exportFilenameInput').value.trim() || `estate_${state.selectedLandIdent}_${scope}`).replace(/\.json$/i, '');
        const targetPath = await Bridge.savePresetDialog(
          I18n.t('dialog.save_preset'),
          state.watchedDirs[0] || null,
          `${rawName}.json`
        );
        if (!targetPath) return;
        const savedPath = await Bridge.exportPresetToFile(state.selectedLandIdent, scope, targetPath);
        toast(I18n.t('presets.export_saved', { path: savedPath }));
        await refreshLocalPresets();
      } catch (err) {
        toast(I18n.t('presets.export_fail', { error: err.message }), true);
      }
    }));

    $('btnExportSaveWatched')?.addEventListener('click', () => runBusy($('btnExportSaveWatched'), async () => {
      if (!state.selectedLandIdent) { toast(I18n.t('presets.need_estate_first'), true); return; }
      if (!state.watchedDirs.length) {
        toast(I18n.t('presets.need_watch_first'), true);
        return;
      }
      try {
        const scope = $('exportScopeSelect').value;
        const rawName = ($('exportFilenameInput').value.trim() || `estate_${state.selectedLandIdent}_${scope}`).replace(/\.json$/i, '');
        const dir = state.watchedDirs[0].replace(/[\\/]+$/, '');
        const targetPath = `${dir}\\${rawName}.json`;
        const savedPath = await Bridge.exportPresetToFile(state.selectedLandIdent, scope, targetPath);
        toast(I18n.t('presets.export_saved', { path: savedPath }));
        await refreshLocalPresets();
        await Bridge.revealInExplorer(savedPath).catch(() => {});
      } catch (err) {
        toast(I18n.t('presets.export_fail', { error: err.message }), true);
      }
    }));

    $('btnExportSaveServer').addEventListener('click', () => runBusy($('btnExportSaveServer'), async () => {
      if (!state.selectedLandIdent) { toast(I18n.t('presets.need_estate_first'), true); return; }
      try {
        const res = await apiJson(`/estates/${state.selectedLandIdent}/export/remakeplace`, {
          method: 'POST',
          body: JSON.stringify({
            scope: $('exportScopeSelect').value,
            save_to_disk: true,
            filename: $('exportFilenameInput').value.trim() || null,
          }),
        });
        toast(I18n.t('presets.export_server_ok', { path: res.saved_path }));
      } catch (err) {
        toast(I18n.t('presets.export_server_fail', { error: err.message }), true);
      }
    }));

    // Furniture Inspector & Transform Editor
    $('btnRefreshFurniture').addEventListener('click', () => refreshBridgeSnapshot({ forceSyncDraft: true }));
    $('btnFurnScopeIndoor').addEventListener('click', () => {
      cancelPendingLiveMove();
      state.furnScope = 'indoor';
      state.selectedFurnSlot = null;
      state.transformDirty = false;
      syncSelectedFurnitureIntoDraft({ force: true });
      renderFurniture();
    });
    $('btnFurnScopeOutdoor').addEventListener('click', () => {
      cancelPendingLiveMove();
      state.furnScope = 'outdoor';
      state.selectedFurnSlot = null;
      state.transformDirty = false;
      syncSelectedFurnitureIntoDraft({ force: true });
      renderFurniture();
    });
    $('tfPlotLocal').addEventListener('change', () => {
      cancelPendingLiveMove();
      const item = getActiveFurnitureList().find((f) => f.flat_slot === state.selectedFurnSlot) || null;
      const d = state.transformDraft;
      const usePlotLocal = state.furnScope === 'outdoor' && $('tfPlotLocal').checked;
      if (
        item
        && state.transformDirty
        && d
        && Number.isFinite(d.x)
        && Number.isFinite(d.y)
        && Number.isFinite(d.z)
        && Number.isFinite(d.rotDeg)
      ) {
        // Convert existing draft between world and plot-local spaces around this item's baseline.
        const theta = item.local_rotation - item.rotation;
        const cos = Math.cos(theta);
        const sin = Math.sin(theta);
        if (usePlotLocal) {
          // Switched world -> plot-local
          const dwx = d.x - item.pos_x;
          const dwy = d.y - item.pos_y;
          const dwz = d.z - item.pos_z;
          const dwr = (d.rotDeg * Math.PI) / 180 - item.rotation;
          state.transformDraft = {
            x: item.local_x + (dwx * cos + dwz * sin),
            y: item.local_y + dwy,
            z: item.local_z + (-dwx * sin + dwz * cos),
            rotDeg: wrapDegrees(((item.local_rotation + dwr) * 180) / Math.PI),
          };
          state.transformOriginal = {
            x: item.local_x,
            y: item.local_y,
            z: item.local_z,
            rotDeg: wrapDegrees((item.local_rotation * 180) / Math.PI),
          };
        } else {
          // Switched plot-local -> world
          const dlx = d.x - item.local_x;
          const dly = d.y - item.local_y;
          const dlz = d.z - item.local_z;
          const dlr = (d.rotDeg * Math.PI) / 180 - item.local_rotation;
          state.transformDraft = {
            x: item.pos_x + (dlx * cos - dlz * sin),
            y: item.pos_y + dly,
            z: item.pos_z + (dlx * sin + dlz * cos),
            rotDeg: wrapDegrees(((item.rotation + dlr) * 180) / Math.PI),
          };
          state.transformOriginal = {
            x: item.pos_x,
            y: item.pos_y,
            z: item.pos_z,
            rotDeg: wrapDegrees((item.rotation * 180) / Math.PI),
          };
        }
      } else {
        syncSelectedFurnitureIntoDraft({ force: true });
      }
      renderFurniture();
    });
    ['furnSearchInput', 'furnDyedOnly', 'furnSortSelect'].forEach((id) => {
      $(id).addEventListener('input', renderFurniture);
      $(id).addEventListener('change', renderFurniture);
    });

    $('furnitureTableBody').addEventListener('click', (ev) => {
      const tr = ev.target.closest('[data-furn-slot]');
      if (!tr) return;
      cancelPendingLiveMove();
      state.selectedFurnSlot = Number(tr.dataset.furnSlot);
      state.transformDirty = false;
      syncSelectedFurnitureIntoDraft({ force: true });
      renderFurniture();
    });

    const onAxisEdit = (axis, value) => {
      if (!state.transformDraft || !Number.isFinite(value)) return;
      state.transformDirty = true;
      if (axis === 'x') state.transformDraft.x = value;
      if (axis === 'y') state.transformDraft.y = value;
      if (axis === 'z') state.transformDraft.z = value;
      if (axis === 'rot') state.transformDraft.rotDeg = wrapDegrees(value);
      if (axis === 'rot') $('tfRotSlider').value = Math.max(-180, Math.min(180, state.transformDraft.rotDeg));
      scheduleLivePreview();
    };

    $('tfPosX').addEventListener('input', (e) => onAxisEdit('x', parseFloat(e.target.value)));
    $('tfPosY').addEventListener('input', (e) => onAxisEdit('y', parseFloat(e.target.value)));
    $('tfPosZ').addEventListener('input', (e) => onAxisEdit('z', parseFloat(e.target.value)));
    $('tfRotDeg').addEventListener('input', (e) => onAxisEdit('rot', parseFloat(e.target.value)));
    $('tfRotSlider').addEventListener('input', (e) => {
      if (!state.transformDraft) return;
      const v = wrapDegrees(parseFloat(e.target.value));
      state.transformDirty = true;
      state.transformDraft.rotDeg = v;
      $('tfRotDeg').value = v.toFixed(1);
      scheduleLivePreview();
    });

    document.querySelectorAll('.btn-nudge').forEach((btn) => {
      btn.addEventListener('click', () => {
        if (!state.transformDraft) return;
        const axis = btn.dataset.axis;
        const delta = parseFloat(btn.dataset.delta);
        state.transformDirty = true;
        if (axis === 'x') state.transformDraft.x = Number((state.transformDraft.x + delta).toFixed(4));
        if (axis === 'y') state.transformDraft.y = Number((state.transformDraft.y + delta).toFixed(4));
        if (axis === 'z') state.transformDraft.z = Number((state.transformDraft.z + delta).toFixed(4));
        if (axis === 'rot') {
          state.transformDraft.rotDeg = Number(wrapDegrees(state.transformDraft.rotDeg + delta).toFixed(2));
        }
        renderFurniture();
        scheduleLivePreview();
      });
    });

    $('btnTfRevert').addEventListener('click', () => {
      if (!state.transformOriginal) return;
      cancelPendingLiveMove();
      state.transformDraft = { ...state.transformOriginal };
      state.transformDirty = false;
      renderFurniture();
      dispatchLiveMove({ commitDb: false }).catch((err) =>
        toast(I18n.t('furniture.revert_err', { error: err.message }), true)
      );
    });
    $('btnTfCommit').addEventListener('click', () => {
      cancelPendingLiveMove();
      runBusy($('btnTfCommit'), () =>
        dispatchLiveMove({ commitDb: true }).catch((err) =>
          toast(I18n.t('furniture.commit_err', { error: err.message }), true)
        )
      );
    });

    // Environment & Snapshots
    document.querySelectorAll('#envLightPills [data-light]').forEach((btn) => {
      btn.addEventListener('click', () => {
        document.querySelectorAll('#envLightPills [data-light]').forEach((b) => b.classList.remove('active'));
        btn.classList.add('active');
        state.envDirty.light = true;
      });
    });
    $('envSsaoCheck').addEventListener('change', () => { state.envDirty.light = true; });
    $('envVisitorsCheck').addEventListener('change', () => { state.envDirty.light = true; });
    $('envStyleSelect').addEventListener('change', () => { state.envDirty.style = true; });

    $('envNameInput').addEventListener('input', (e) => {
      state.envDirty.meta = true;
      $('envNameBytes').textContent = utf8ByteLength(e.target.value);
    });
    $('envGreetingInput').addEventListener('input', (e) => {
      state.envDirty.meta = true;
      $('envGreetingBytes').textContent = utf8ByteLength(e.target.value);
    });
    $('envMemoInput').addEventListener('input', () => { state.envDirty.memo = true; });

    $('btnEnvSaveLight').addEventListener('click', () => {
      if (!state.selectedLandIdent) return;
      runBusy($('btnEnvSaveLight'), async () => {
        const activeBtn = document.querySelector('#envLightPills .seg-btn.active');
        const lightLevel = activeBtn ? Number(activeBtn.dataset.light) : 3;
        try {
          await apiJson(`/estates/${state.selectedLandIdent}`, {
            method: 'PATCH',
            body: JSON.stringify({
              light_level: lightLevel,
              enable_ssao: $('envSsaoCheck').checked,
              visitors_allowed: $('envVisitorsCheck').checked,
              reload_mode: $('envReloadSelect').value,
              source: 'web_ui',
            }),
          });
          state.envDirty.light = false;
          toast(I18n.t('env.light_saved', { level: lightLevel }));
          await refreshEstateDetail({ silent: true, forceSync: false });
        } catch (err) {
          toast(I18n.t('env.light_fail', { error: err.message }), true);
        }
      });
    });

    $('btnEnvApplyStyle').addEventListener('click', () => {
      if (!state.selectedLandIdent) return;
      runBusy($('btnEnvApplyStyle'), async () => {
        try {
          await apiJson(`/estates/${state.selectedLandIdent}`, {
            method: 'PATCH',
            body: JSON.stringify({
              interior_style: $('envStyleSelect').value,
              reload_mode: 'zone_reload',
              source: 'web_ui',
            }),
          });
          state.envDirty.style = false;
          toast(I18n.t('env.style_applied'));
          await refreshAll(state.selectedLandIdent, { forceSync: false });
        } catch (err) {
          toast(I18n.t('env.style_fail', { error: err.message }), true);
        }
      });
    });

    $('btnEnvSaveMeta').addEventListener('click', () => {
      if (!state.selectedLandIdent) return;
      runBusy($('btnEnvSaveMeta'), async () => {
        try {
          await apiJson(`/estates/${state.selectedLandIdent}`, {
            method: 'PATCH',
            body: JSON.stringify({
              estate_name: $('envNameInput').value.trim(),
              greeting: $('envGreetingInput').value.trim(),
              reload_mode: 'live_packets',
              source: 'web_ui',
            }),
          });
          const memo = $('envMemoInput').value.trim();
          if (memo) state.memos[state.selectedLandIdent] = memo;
          else delete state.memos[state.selectedLandIdent];
          savePrefs();
          state.envDirty.meta = false;
          state.envDirty.memo = false;
          toast(I18n.t('env.meta_saved'));
          await refreshAll(state.selectedLandIdent, { forceSync: false });
        } catch (err) {
          toast(I18n.t('env.meta_fail', { error: err.message }), true);
        }
      });
    });

    $('btnEnvSaveMemo').addEventListener('click', () => {
      if (!state.selectedLandIdent) return;
      const memo = $('envMemoInput').value.trim();
      if (memo) state.memos[state.selectedLandIdent] = memo;
      else delete state.memos[state.selectedLandIdent];
      savePrefs();
      state.envDirty.memo = false;
      renderEstates();
      toast(I18n.t('env.memo_saved'));
    });

    $('btnResetIndoor').addEventListener('click', () => {
      if (state.selectedLandIdent) {
        runBusy($('btnResetIndoor'), () => resetEstateScope(state.selectedLandIdent, 'interior'));
      }
    });
    $('btnResetExterior').addEventListener('click', () => {
      if (state.selectedLandIdent) {
        runBusy($('btnResetExterior'), () => resetEstateScope(state.selectedLandIdent, 'exterior'));
      }
    });
    $('btnResetAll').addEventListener('click', () => {
      if (state.selectedLandIdent) {
        runBusy($('btnResetAll'), () => resetEstateScope(state.selectedLandIdent, 'all'));
      }
    });
    $('btnDeleteEstate').addEventListener('click', () => {
      if (state.selectedLandIdent) {
        runBusy($('btnDeleteEstate'), () => deleteEstate(state.selectedLandIdent));
      }
    });
    $('btnEnvForceReload').addEventListener('click', () => reloadSelectedEstate($('btnEnvForceReload')));

    $('btnSnapshotCreate').addEventListener('click', () => {
      if (!state.selectedLandIdent) return;
      runBusy($('btnSnapshotCreate'), async () => {
        try {
          const res = await apiJson(`/estates/${state.selectedLandIdent}/snapshots`, {
            method: 'POST',
            body: JSON.stringify({ label: $('snapshotLabelInput').value.trim() || null }),
          });
          $('snapshotLabelInput').value = '';
          toast(I18n.t('env.snap_created', { id: res.id }));
          await refreshEstateDetail({ silent: true, forceSync: false });
        } catch (err) {
          toast(I18n.t('env.snap_create_fail', { error: err.message }), true);
        }
      });
    });

    $('btnSnapshotUndo').addEventListener('click', () => undoSelectedEstate($('btnSnapshotUndo')));

    $('snapshotTimeline').addEventListener('click', (ev) => {
      const btn = ev.target.closest('[data-restore-snapshot]');
      if (!btn || !state.selectedLandIdent) return;
      const snapId = Number(btn.dataset.restoreSnapshot);
      runBusy(btn, async () => {
        const ok = await askConfirm(
          I18n.t('env.restore_confirm_title'),
          I18n.t('env.restore_confirm_msg', { id: snapId })
        );
        if (!ok) return;
        try {
          await apiJson(`/estates/${state.selectedLandIdent}/snapshots/${snapId}/restore`, {
            method: 'POST',
            body: JSON.stringify({ reload_mode: 'auto', preserve_pos: true, source: 'web_ui' }),
          });
          toast(I18n.t('env.restore_ok', { id: snapId }));
          await refreshAll(state.selectedLandIdent, { forceSync: true });
        } catch (err) {
          toast(I18n.t('env.restore_fail', { error: err.message }), true);
        }
      });
    });

    // Logs
    $('logCategoryFilter').addEventListener('click', (ev) => {
      const btn = ev.target.closest('[data-log-cat]');
      if (!btn) return;
      state.logCategory = btn.dataset.logCat;
      document.querySelectorAll('#logCategoryFilter [data-log-cat]').forEach((b) => {
        b.classList.toggle('active', b === btn);
      });
      renderLogs();
    });
    $('logsSearchInput').addEventListener('input', renderLogs);
    $('btnClearLogs').addEventListener('click', () => { state.logs = []; renderLogs(); });
    $('btnCopyLogs').addEventListener('click', async () => {
      const text = state.logs.map((l) => `[${l.time}] [${l.kind}] ${l.message}`).join('\n');
      try {
        await navigator.clipboard.writeText(text);
        toast(I18n.t('logs.copied'));
      } catch {
        toast(I18n.t('logs.copy_fail'), true);
      }
    });
  }

  // ---- Init ----

  window.FoundationStudioApp = {
    switchTab,
    refreshAll,
    refreshLocalPresets,
    toast,
    pushExternalLog: (kind, source, message) => pushLog({ kind, category: 'launcher', source, message }),
    syncWatchedSettings: (dirs, autoApply) => {
      state.watchedDirs = Array.isArray(dirs) ? [...dirs] : [];
      state.autoApplyWatched = !!autoApply;
      if ($('presetAutoApplyCheck')) $('presetAutoApplyCheck').checked = state.autoApplyWatched;
      renderWatchedDirs();
    },
  };

  async function boot() {
    let uiLocale = 'auto';
    try {
      const settings = await Bridge.getSettings();
      uiLocale = settings?.uiLocale || 'auto';
    } catch {
      // Fall back to system locale detection inside FoundationI18n.
    }
    await I18n.init(uiLocale);
    I18n.onChange(() => {
      setSseConnected(state.sseConnected);
      renderWatchedDirs();
      renderAll();
    });

    loadPrefs();
    bindEvents();
    switchTab(state.activeTab);
    connectSse();
    connectWatcherAndDropEvents();
    refreshLocalPresets();
    refreshAll(null, { forceSync: true, silentError: true });
  }

  boot().catch((err) => {
    console.error('Studio init error:', err);
  });
})();
