const NATIVE_HOST = 'com.victauri.browser';
const COMMAND_TIMEOUT_MS = 30000;

let nativePort = null;
let pendingCommands = new Map();
let tabStates = new Map();
let cdpAttached = new Map();
let cdpDetachTimers = new Map();
const CDP_DETACH_DELAY = 5000;

function connectNative() {
    if (nativePort) return;
    try {
        nativePort = chrome.runtime.connectNative(NATIVE_HOST);
        nativePort.onMessage.addListener(onNativeMessage);
        nativePort.onDisconnect.addListener(onNativeDisconnect);
        console.log('[victauri] Connected to native host');
    } catch (e) {
        console.error('[victauri] Failed to connect:', e);
        scheduleReconnect();
    }
}

function onNativeDisconnect() {
    const error = chrome.runtime.lastError;
    console.warn('[victauri] Native host disconnected:', error?.message || 'unknown');
    nativePort = null;

    for (const [id, entry] of pendingCommands) {
        entry.reject(new Error('Native host disconnected'));
        pendingCommands.delete(id);
    }

    scheduleReconnect();
}

function scheduleReconnect() {
    chrome.alarms.create('victauri-reconnect', { delayInMinutes: 0.4 });
}

chrome.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === 'victauri-reconnect') {
        connectNative();
    }
});

function onNativeMessage(message) {
    if (message.type === 'execute' || message.type === 'cdp') {
        handleHostCommand(message);
    }
}

async function handleHostCommand(command) {
    const { id, type: cmdType, tab_id, method, args, domain_method, params } = command;

    try {
        let tabId = tab_id;
        if (!tabId) {
            const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
            if (!activeTab) {
                sendToHost({ id, type: 'error', error: 'No active tab' });
                return;
            }
            tabId = activeTab.id;
        }

        if (cmdType === 'cdp') {
            const result = await executeCdp(tabId, domain_method, params);
            sendToHost({ id, type: 'response', data: result });
        } else if (method === 'screenshot') {
            const data = await captureScreenshot(tabId, args || {});
            sendToHost({ id, type: 'response', data });
        } else {
            const result = await sendToContentScript(tabId, id, method, args);
            sendToHost({ id, type: 'response', data: result });
        }
    } catch (e) {
        sendToHost({ id, type: 'response', error: e.message });
    }
}

function sendToHost(message) {
    if (!nativePort) return;
    try {
        nativePort.postMessage(message);
    } catch (e) {
        console.error('[victauri] sendToHost failed:', e);
    }
}

async function sendToContentScript(tabId, commandId, method, args) {
    return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
            reject(new Error(`Bridge timeout (${COMMAND_TIMEOUT_MS}ms) for ${method}`));
        }, COMMAND_TIMEOUT_MS);

        chrome.tabs.sendMessage(
            tabId,
            { type: 'victauri_command', id: commandId, method, args },
            (response) => {
                clearTimeout(timeout);
                if (chrome.runtime.lastError) {
                    reject(new Error(chrome.runtime.lastError.message));
                    return;
                }
                if (!response) {
                    reject(new Error('No response from content script'));
                    return;
                }
                if (response.type === 'error') {
                    reject(new Error(response.error));
                } else {
                    resolve(response.data);
                }
            }
        );
    });
}

async function captureScreenshot(tabId, options) {
    const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!options.fullPage && activeTab && activeTab.id === tabId) {
        const dataUrl = await chrome.tabs.captureVisibleTab(null, { format: 'png' });
        return dataUrl.split(',')[1];
    }

    await ensureCdpAttached(tabId);
    const result = await chrome.debugger.sendCommand(
        { tabId },
        'Page.captureScreenshot',
        { format: 'png', captureBeyondViewport: options.fullPage ?? false }
    );
    scheduleCdpDetach(tabId);
    return result.data;
}

async function executeCdp(tabId, domainMethod, params) {
    await ensureCdpAttached(tabId);
    const result = await chrome.debugger.sendCommand({ tabId }, domainMethod, params || {});
    scheduleCdpDetach(tabId);
    return result;
}

async function ensureCdpAttached(tabId) {
    if (cdpAttached.has(tabId)) {
        const timer = cdpDetachTimers.get(tabId);
        if (timer) {
            clearTimeout(timer);
            cdpDetachTimers.delete(tabId);
        }
        return;
    }

    await chrome.debugger.attach({ tabId }, '1.3');
    cdpAttached.set(tabId, true);
}

function scheduleCdpDetach(tabId) {
    const existing = cdpDetachTimers.get(tabId);
    if (existing) clearTimeout(existing);

    const timer = setTimeout(async () => {
        cdpDetachTimers.delete(tabId);
        cdpAttached.delete(tabId);
        try {
            await chrome.debugger.detach({ tabId });
        } catch (e) {
            // Tab may have been closed
        }
    }, CDP_DETACH_DELAY);

    cdpDetachTimers.set(tabId, timer);
}

// Tab lifecycle tracking
chrome.tabs.onCreated.addListener((tab) => {
    tabStates.set(tab.id, { url: tab.url || '', title: tab.title || '', bridgeReady: false });
    sendToHost({ type: 'tab_created', tab_id: tab.id, url: tab.url, title: tab.title });
});

chrome.tabs.onRemoved.addListener((tabId) => {
    tabStates.delete(tabId);
    cdpAttached.delete(tabId);
    const timer = cdpDetachTimers.get(tabId);
    if (timer) {
        clearTimeout(timer);
        cdpDetachTimers.delete(tabId);
    }
    sendToHost({ type: 'tab_closed', tab_id: tabId });
});

chrome.tabs.onActivated.addListener(({ tabId }) => {
    sendToHost({ type: 'tab_activated', tab_id: tabId });
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
    if (changeInfo.url || changeInfo.title) {
        const state = tabStates.get(tabId) || {};
        if (changeInfo.url) state.url = changeInfo.url;
        if (changeInfo.title) state.title = changeInfo.title;
        tabStates.set(tabId, state);
        sendToHost({
            type: 'tab_updated',
            tab_id: tabId,
            url: changeInfo.url,
            title: changeInfo.title,
        });
    }
});

// Content script ready handler
chrome.runtime.onMessage.addListener((message, sender) => {
    if (message.type === 'content_script_ready' && sender.tab) {
        const tabId = sender.tab.id;
        const state = tabStates.get(tabId) || {};
        state.bridgeReady = true;
        tabStates.set(tabId, state);
        sendToHost({ type: 'bridge_ready', tab_id: tabId, url: message.url });
    }
});

// Sync existing tabs on startup (service worker may restart)
async function syncExistingTabs() {
    try {
        const tabs = await chrome.tabs.query({});
        for (const tab of tabs) {
            if (!tabStates.has(tab.id)) {
                tabStates.set(tab.id, {
                    url: tab.url || '',
                    title: tab.title || '',
                    bridgeReady: false,
                });
                sendToHost({
                    type: 'tab_created',
                    tab_id: tab.id,
                    url: tab.url || '',
                    title: tab.title || '',
                });
            }
        }
        const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (activeTab) {
            sendToHost({ type: 'tab_activated', tab_id: activeTab.id });
        }
    } catch (e) {
        console.error('[victauri] Tab sync failed:', e);
    }
}

// Connect on startup
connectNative();
syncExistingTabs();
