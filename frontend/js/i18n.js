(() => {
  'use strict';

  const SUPPORTED_LOCALES = ['ko', 'en', 'ja', 'de', 'fr'];
  const DEFAULT_PREFERENCE = 'auto';
  const DEFAULT_GAME_DATA_PREFERENCE = 'follow_client';
  const GAME_TERM_PREFIXES = ['game.districts.', 'game.styles.', 'game.zones.', 'game.expansions.'];

  const dicts = {};
  const listeners = new Set();
  let loadPromise = null;
  let initPromise = null;
  let currentPreference = DEFAULT_PREFERENCE;
  let activeLocale = 'en';
  let gameDataPreference = DEFAULT_GAME_DATA_PREFERENCE;
  let clientLanguage = 0;
  let activeGameLocale = 'ja';
  let lastGameDataInfo = null;
  let gameNameCache = { locale: 'ja', items: {}, stains: {}, byName: {} };

  function normalizeLocaleTag(tag) {
    const primary = String(tag || '')
      .trim()
      .toLowerCase()
      .split(/[-_.]/)[0];
    return SUPPORTED_LOCALES.includes(primary) ? primary : 'en';
  }

  function normalizePreference(pref) {
    const raw = String(pref || '').trim().toLowerCase();
    if (!raw || raw === 'auto') {
      return 'auto';
    }
    return normalizeLocaleTag(raw);
  }

  function clientLanguageToLocale(langNum) {
    switch (Number(langNum)) {
      case 0:
        return 'ja';
      case 1:
        return 'en';
      case 2:
        return 'de';
      case 3:
        return 'fr';
      default:
        return 'en';
    }
  }

  function normalizeGameDataPreference(pref) {
    const raw = String(pref || '').trim().toLowerCase();
    if (!raw || raw === 'follow_client') {
      return 'follow_client';
    }
    if (raw === 'follow_ui') {
      return 'follow_ui';
    }
    return normalizeLocaleTag(raw);
  }

  function computeActiveGameLocale() {
    if (gameDataPreference === 'follow_client') {
      return clientLanguageToLocale(clientLanguage);
    }
    if (gameDataPreference === 'follow_ui') {
      return activeLocale;
    }
    return normalizeLocaleTag(gameDataPreference);
  }

  function resetGameNameCache() {
    gameNameCache = { locale: activeGameLocale, items: {}, stains: {}, byName: {} };
  }

  function isGameTermKey(key) {
    const s = String(key || '');
    return GAME_TERM_PREFIXES.some((prefix) => s.startsWith(prefix));
  }

  async function loadDictionaries() {
    if (loadPromise) {
      return loadPromise;
    }
    loadPromise = Promise.all(
      SUPPORTED_LOCALES.map(async (code) => {
        try {
          const resp = await fetch(`./locales/${code}.json`);
          if (!resp.ok) {
            throw new Error(`HTTP ${resp.status}`);
          }
          dicts[code] = await resp.json();
        } catch (err) {
          console.error(`Failed to load locale ${code}:`, err);
          dicts[code] = {};
        }
      })
    );
    return loadPromise;
  }

  function hasKeyIn(localeCode, key) {
    return (
      Object.prototype.hasOwnProperty.call(dicts[localeCode] || {}, key) ||
      Object.prototype.hasOwnProperty.call(dicts.en || {}, key)
    );
  }

  function hasKey(key) {
    return hasKeyIn(activeLocale, key);
  }

  function translateFromDict(localeCode, key, params = {}) {
    const raw =
      (dicts[localeCode] && dicts[localeCode][key]) ??
      (dicts.en && dicts.en[key]) ??
      key;
    if (!params || typeof params !== 'object') {
      return String(raw);
    }
    return String(raw).replace(/\{([a-zA-Z0-9_]+)\}/g, (match, name) => {
      if (!Object.prototype.hasOwnProperty.call(params, name)) {
        return match;
      }
      const val = params[name];
      return val === undefined || val === null ? match : String(val);
    });
  }

  function t(key, params = {}) {
    return translateFromDict(activeLocale, key, params);
  }

  function tGame(key, params = {}) {
    return translateFromDict(activeGameLocale, key, params);
  }

  function populateLocaleSelectOptions(root = document) {
    root.querySelectorAll('[data-locale-option]').forEach((opt) => {
      const code = opt.dataset.localeOption;
      const nativeName = dicts[code]?.['meta.name'];
      if (nativeName) {
        opt.textContent = nativeName;
      }
    });
  }

  function syncSelectControls() {
    const uiSel = document.getElementById('prefUiLocaleSelect');
    if (uiSel) {
      uiSel.value = currentPreference;
    }
    const gameSel = document.getElementById('prefGameDataLocaleSelect');
    if (gameSel) {
      gameSel.value = gameDataPreference;
    }
  }

  function notifyListeners() {
    const payload = {
      preference: currentPreference,
      locale: activeLocale,
      gamePreference: gameDataPreference,
      gameLocale: activeGameLocale,
    };
    listeners.forEach((fn) => {
      try {
        fn(payload);
      } catch (err) {
        console.error('i18n onChange listener error:', err);
      }
    });
  }

  function applyDom(root = document) {
    document.documentElement.lang = activeLocale;
    populateLocaleSelectOptions(root);

    root.querySelectorAll('[data-i18n]').forEach((el) => {
      const key = el.dataset.i18n;
      el.textContent = isGameTermKey(key) ? tGame(key) : t(key);
    });
    root.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
      el.placeholder = t(el.dataset.i18nPlaceholder);
    });
    root.querySelectorAll('[data-i18n-title]').forEach((el) => {
      el.title = t(el.dataset.i18nTitle);
    });
    root.querySelectorAll('[data-i18n-aria-label]').forEach((el) => {
      el.setAttribute('aria-label', t(el.dataset.i18nAriaLabel));
    });
    syncSelectControls();
  }

  async function resolveInitialLocale(preference) {
    const pref = normalizePreference(preference);
    currentPreference = pref;
    if (pref !== 'auto') {
      activeLocale = pref;
      return;
    }
    try {
      const Bridge = window.FoundationBridge;
      if (Bridge && typeof Bridge.getSystemLocale === 'function') {
        activeLocale = normalizeLocaleTag(await Bridge.getSystemLocale());
        return;
      }
    } catch {
      // Fallback to WebView navigator language.
    }
    activeLocale = normalizeLocaleTag(navigator.language);
  }

  function init(
    preference = DEFAULT_PREFERENCE,
    gameDataPref = DEFAULT_GAME_DATA_PREFERENCE,
    clientLang = 0
  ) {
    if (initPromise) {
      return initPromise;
    }
    initPromise = (async () => {
      await loadDictionaries();
      await resolveInitialLocale(preference);
      gameDataPreference = normalizeGameDataPreference(gameDataPref);
      clientLanguage = Number(clientLang) || 0;
      activeGameLocale = computeActiveGameLocale();
      resetGameNameCache();
      applyDom(document);
      return {
        preference: currentPreference,
        locale: activeLocale,
        gamePreference: gameDataPreference,
        gameLocale: activeGameLocale,
      };
    })();
    return initPromise;
  }

  async function setLocale(preference) {
    await loadDictionaries();
    const pref = normalizePreference(preference);
    const Bridge = window.FoundationBridge;
    let resolved = pref;
    if (Bridge && typeof Bridge.setUiLocale === 'function') {
      resolved = await Bridge.setUiLocale(pref);
    } else if (pref === 'auto') {
      resolved = normalizeLocaleTag(navigator.language);
    }
    currentPreference = pref;
    activeLocale = normalizeLocaleTag(resolved);
    if (gameDataPreference === 'follow_ui') {
      const nextGameLocale = computeActiveGameLocale();
      if (nextGameLocale !== activeGameLocale) {
        activeGameLocale = nextGameLocale;
        resetGameNameCache();
      }
    }
    applyDom(document);
    notifyListeners();
    return {
      preference: currentPreference,
      locale: activeLocale,
      gamePreference: gameDataPreference,
      gameLocale: activeGameLocale,
    };
  }

  async function setGameDataLocale(preference, clientLang = clientLanguage) {
    await loadDictionaries();
    gameDataPreference = normalizeGameDataPreference(preference);
    clientLanguage = Number(clientLang) || 0;
    const Bridge = window.FoundationBridge;
    let info = null;
    if (Bridge && typeof Bridge.setGameDataLocale === 'function') {
      info = await Bridge.setGameDataLocale(gameDataPreference);
      lastGameDataInfo = info;
      activeGameLocale = normalizeLocaleTag(info?.requestedLocale || computeActiveGameLocale());
    } else {
      activeGameLocale = computeActiveGameLocale();
    }
    resetGameNameCache();
    applyDom(document);
    notifyListeners();
    return info;
  }

  function syncClientLanguage(clientLang) {
    const nextLang = Number(clientLang) || 0;
    if (nextLang === clientLanguage && gameNameCache.locale === computeActiveGameLocale()) {
      return;
    }
    clientLanguage = nextLang;
    if (gameDataPreference === 'follow_client') {
      const nextGameLocale = computeActiveGameLocale();
      if (nextGameLocale !== activeGameLocale) {
        activeGameLocale = nextGameLocale;
        resetGameNameCache();
        applyDom(document);
        notifyListeners();
      }
    }
  }

  async function refreshGameDataInfo() {
    const Bridge = window.FoundationBridge;
    if (!Bridge || typeof Bridge.getGameDataInfo !== 'function') {
      return lastGameDataInfo;
    }
    const info = await Bridge.getGameDataInfo();
    lastGameDataInfo = info || null;
    if (info?.requestedLocale) {
      const nextGameLocale = normalizeLocaleTag(info.requestedLocale);
      if (nextGameLocale !== activeGameLocale) {
        activeGameLocale = nextGameLocale;
        resetGameNameCache();
        applyDom(document);
        notifyListeners();
      }
    }
    return info;
  }

  async function resolveGameNames(itemIds = [], stainIds = [], names = []) {
    const Bridge = window.FoundationBridge;
    if (!Bridge || typeof Bridge.resolveGameNames !== 'function') {
      return false;
    }
    if (lastGameDataInfo && !lastGameDataInfo.gameDirValid) {
      return false;
    }
    if (gameNameCache.locale !== activeGameLocale) {
      resetGameNameCache();
    }

    const reqItems = [];
    const seenItems = new Set();
    for (const raw of itemIds || []) {
      const id = Number(raw);
      if (Number.isInteger(id) && id > 0 && !seenItems.has(id)) {
        const key = String(id);
        if (!Object.prototype.hasOwnProperty.call(gameNameCache.items, key)) {
          seenItems.add(id);
          reqItems.push(id);
        }
      }
    }

    const reqStains = [];
    const seenStains = new Set();
    for (const raw of stainIds || []) {
      const id = Number(raw);
      if (Number.isInteger(id) && id > 0 && id <= 255 && !seenStains.has(id)) {
        const key = String(id);
        if (!Object.prototype.hasOwnProperty.call(gameNameCache.stains, key)) {
          seenStains.add(id);
          reqStains.push(id);
        }
      }
    }

    const reqNames = [];
    const seenNames = new Set();
    for (const raw of names || []) {
      const s = String(raw || '').trim();
      if (s && !seenNames.has(s) && !Object.prototype.hasOwnProperty.call(gameNameCache.byName, s)) {
        seenNames.add(s);
        reqNames.push(s);
      }
    }

    if (!reqItems.length && !reqStains.length && !reqNames.length) {
      return false;
    }

    const cacheRef = gameNameCache;
    const res = await Bridge.resolveGameNames(reqItems, reqStains, reqNames);
    if (!res || gameNameCache !== cacheRef) {
      return false;
    }

    let changed = false;
    const retItems = res.items || {};
    for (const id of reqItems) {
      const key = String(id);
      const val = typeof retItems[key] === 'string' && retItems[key] ? retItems[key] : null;
      if (val && gameNameCache.items[key] !== val) {
        changed = true;
      }
      gameNameCache.items[key] = val;
    }

    const retStains = res.stains || {};
    for (const id of reqStains) {
      const key = String(id);
      const val = typeof retStains[key] === 'string' && retStains[key] ? retStains[key] : null;
      if (val && gameNameCache.stains[key] !== val) {
        changed = true;
      }
      gameNameCache.stains[key] = val;
    }

    const retByName = res.byName || {};
    for (const name of reqNames) {
      const val = typeof retByName[name] === 'string' && retByName[name] ? retByName[name] : null;
      if (val && gameNameCache.byName[name] !== val) {
        changed = true;
      }
      gameNameCache.byName[name] = val;
    }

    return changed;
  }

  function getItemName(itemId, fallbackName = '') {
    const id = Number(itemId);
    if (Number.isInteger(id) && id > 0) {
      const hit = gameNameCache.items[String(id)];
      if (hit) {
        return hit;
      }
    }
    const fb = String(fallbackName || '').trim();
    if (fb && gameNameCache.byName[fb]) {
      return gameNameCache.byName[fb];
    }
    return fb;
  }

  function getStainName(stainId, fallback = '') {
    const id = Number(stainId);
    if (Number.isInteger(id) && id > 0) {
      const hit = gameNameCache.stains[String(id)];
      if (hit) {
        return hit;
      }
    }
    return fallback;
  }

  function onChange(fn) {
    if (typeof fn !== 'function') {
      return () => {};
    }
    listeners.add(fn);
    return () => listeners.delete(fn);
  }

  function canonicalStyleKey(raw) {
    if (raw === null || raw === undefined) return '';
    const trimmed = String(raw).trim();
    if (!trimmed) return '';
    const lower = trimmed.toLowerCase();
    if (
      lower === 'dark_minimalist' ||
      (lower.includes('dark') && lower.includes('minimal')) ||
      lower.includes('schlicht & dunkel') ||
      lower.includes('sobre (noir)') ||
      trimmed.includes('\u30E2\u30CE\u30C8\u30FC\u30F3')
    ) {
      return 'dark_minimalist';
    }
    if (
      lower === 'minimalist' ||
      lower.includes('minimal') ||
      lower.includes('schlicht') ||
      lower.includes('sobre') ||
      trimmed.includes('\u30B7\u30F3\u30D7\u30EB')
    ) {
      return 'minimalist';
    }
    if (
      lower === 'lavender_beds' ||
      lower.includes('lavender') ||
      lower.includes('lavendel') ||
      lower.includes('lavandi') ||
      trimmed.includes('\u30E9\u30D9\u30F3\u30C0\u30FC')
    ) {
      return 'lavender_beds';
    }
    if (
      lower === 'mist' ||
      lower.includes('mist') ||
      lower.includes('dorf des nebels') ||
      lower.includes('brum') ||
      trimmed.includes('\u30DF\u30B9\u30C8')
    ) {
      return 'mist';
    }
    if (
      lower === 'goblet' ||
      lower.includes('goblet') ||
      lower.includes('kelchkuppe') ||
      lower.includes('coupe') ||
      trimmed.includes('\u30B4\u30D6\u30EC\u30C3\u30C8')
    ) {
      return 'goblet';
    }
    if (lower === 'shirogane' || lower.includes('shirogane') || trimmed.includes('\u30B7\u30ED\u30AC\u30CD')) {
      return 'shirogane';
    }
    if (
      lower === 'empyreum' ||
      lower.includes('empyreum') ||
      lower.includes('empyr') ||
      lower.includes('ishgard') ||
      trimmed.includes('\u30A8\u30F3\u30D4\u30EC\u30A2\u30E0')
    ) {
      return 'empyreum';
    }
    if (lower === 'unspecified') return 'unspecified';
    if (lower === 'unknown') return 'unknown';
    return '';
  }

  function formatDistrict(tid) {
    const key = `game.districts.${tid}`;
    if (hasKeyIn(activeGameLocale, key)) {
      return tGame(key);
    }
    return t('game.district_fallback', { tid });
  }

  function formatZone(zoneId) {
    const key = `game.zones.${zoneId}`;
    if (hasKeyIn(activeGameLocale, key)) {
      return tGame(key);
    }
    return t('game.zone_fallback', { zoneId });
  }

  function formatSize(plotSize, isApartment) {
    if (isApartment) return t('game.sizes.apartment');
    if (plotSize === 0) return t('game.sizes.small');
    if (plotSize === 1) return t('game.sizes.medium');
    if (plotSize === 2) return t('game.sizes.large');
    return t('game.sizes.other');
  }

  function formatSizeKey(sizeKey, rawFallback = '') {
    if (['small', 'medium', 'large', 'apartment', 'unknown'].includes(sizeKey)) {
      return t(`game.sizes.${sizeKey}`);
    }
    const trimmed = String(rawFallback || '').trim();
    return trimmed || t('game.sizes.other');
  }

  function formatStyle(styleKeyOrRaw, fallbackKey = 'game.styles.default', rawFallback = '') {
    const val = String(styleKeyOrRaw ?? '').trim();
    const rawTrimmed = String(rawFallback ?? '').trim();
    if (!val) {
      return rawTrimmed || t(fallbackKey);
    }
    if (val === 'unspecified') {
      return t('game.styles.unspecified');
    }
    if (val === 'unknown') {
      return rawTrimmed || t('game.styles.unspecified');
    }
    const key = canonicalStyleKey(val);
    if (key && key !== 'unknown') {
      return tGame(`game.styles.${key}`);
    }
    return val;
  }

  function normalizeStyleSelectValue(raw) {
    const key = canonicalStyleKey(raw);
    switch (key) {
      case 'dark_minimalist':
        return 'dark minimalist';
      case 'minimalist':
        return 'minimalist';
      case 'mist':
        return 'mist';
      case 'lavender_beds':
        return 'lavender beds';
      case 'goblet':
        return 'goblet';
      case 'shirogane':
        return 'shirogane';
      case 'empyreum':
        return 'empyreum';
      default:
        return 'lavender beds';
    }
  }

  function mapFixtureKindToKey(kind) {
    switch (String(kind || '').trim().toLowerCase()) {
      case 'wall':
      case 'walls':
        return 'wall';
      case 'floor':
      case 'flooring':
        return 'flooring';
      case 'ceiling':
      case 'light':
      case 'lighting':
      case 'chandelier':
        return 'ceiling';
      case 'upper wall':
        return 'upper_wall';
      case 'upper flooring':
        return 'upper_flooring';
      case 'basement wall':
        return 'basement_wall';
      case 'basement flooring':
        return 'basement_flooring';
      case 'roof':
        return 'roof';
      case 'exterior wall':
        return 'exterior_wall';
      case 'window':
      case 'windows':
        return 'window';
      case 'door':
        return 'door';
      case 'fence':
        return 'fence';
      case 'awning':
        return 'awning';
      case 'sign':
      case 'placard':
        return 'sign';
      case 'roof decoration':
      case 'roof decor':
        return 'roof_decoration';
      case 'wall decoration':
      case 'exterior wall decor':
      case 'wall decor':
        return 'wall_decoration';
      default:
        return '';
    }
  }

  function formatFixture(entry) {
    if (!entry) return '';
    if (typeof entry === 'string') {
      const m = entry.match(/^([^:]+):\s*(.+)$/);
      if (!m) return entry;
      const mapped = mapFixtureKindToKey(m[1]);
      if (!mapped) return entry;
      return `${t(`game.fixtures.${mapped}`)}: ${m[2]}`;
    }
    const kindKey = entry.kindKey || mapFixtureKindToKey(entry.rawKind || entry.type);
    const label = kindKey ? t(`game.fixtures.${kindKey}`) : String(entry.rawKind || entry.type || '').trim();
    const name = String(entry.name || '').trim();
    const color = String(entry.color || '').trim();
    const suffix = color ? ` (${color})` : '';
    return label ? `${label}: ${name}${suffix}` : `${name}${suffix}`;
  }

  function formatEstateLocation(row) {
    const district = formatDistrict(row.territory_type_id);
    const ward = (row.ward_index ?? 0) + 1;
    if (row.is_apartment) {
      const division = row.division !== 0 ? t('game.location.subdivision') : t('game.location.main_division');
      return t('game.location.apartment_fmt', {
        district,
        ward,
        division,
        room: row.room_number,
      });
    }
    const plot = (row.division ?? 0) * 30 + (row.plot_index ?? 0) + 1;
    return t('game.location.plot_fmt', {
      district,
      ward,
      plot,
    });
  }

  function formatPreflightIssue(issue) {
    if (!issue) return '';
    if (typeof issue === 'string') return issue;
    if (issue.code) {
      return t(`server.preflight.${issue.code}`, { detail: issue.detail || '-' });
    }
    return String(issue);
  }

  function formatExpansion(level) {
    const key = `game.expansions.${level}`;
    return hasKeyIn(activeGameLocale, key) ? tGame(key) : String(level);
  }

  window.FoundationI18n = {
    SUPPORTED_LOCALES,
    init,
    ready: () => initPromise || init(DEFAULT_PREFERENCE),
    setLocale,
    setGameDataLocale,
    syncClientLanguage,
    refreshGameDataInfo,
    resolveGameNames,
    getItemName,
    getStainName,
    onChange,
    t,
    tGame,
    locale: () => activeLocale,
    preference: () => currentPreference,
    gameLocale: () => activeGameLocale,
    gamePreference: () => gameDataPreference,
    gameDataInfo: () => lastGameDataInfo,
    bcp47: () => t('meta.bcp47') || 'en-US',
    applyDom,
    formatDistrict,
    formatZone,
    formatSize,
    formatSizeKey,
    formatStyle,
    normalizeStyleSelectValue,
    canonicalStyleKey,
    formatFixture,
    formatEstateLocation,
    formatPreflightIssue,
    formatExpansion,
    mapFixtureKindToKey,
  };
})();
