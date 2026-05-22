// Firefox uses the `browser` namespace (Promise-based).
// Provide a compatibility shim for any code that references `chrome`.
const api = typeof browser !== 'undefined' ? browser : chrome;

const NATIVE_HOST = 'com.victauri.browser';
const COMMAND_TIMEOUT_MS = 30000;

let nativePort = null;
let pendingCommands = new Map();
let tabStates = new Map();

function connectNative() {
    if (nativePort) return;
    try {
        nativePort = api.runtime.connectNative(NATIVE_HOST);
        nativePort.onMessage.addListener(onNativeMessage);
        nativePort.onDisconnect.addListener(onNativeDisconnect);
        console.log('[victauri] Connected to native host');
    } catch (e) {
        console.error('[victauri] Failed to connect:', e);
        scheduleReconnect();
    }
}

function onNativeDisconnect() {
    const error = nativePort ? nativePort.error : null;
    console.warn('[victauri] Native host disconnected:', error?.message || 'unknown');
    nativePort = null;

    for (const [id, entry] of pendingCommands) {
        entry.reject(new Error('Native host disconnected'));
        pendingCommands.delete(id);
    }

    scheduleReconnect();
}

function scheduleReconnect() {
    api.alarms.create('victauri-reconnect', { delayInMinutes: 0.4 });
}

api.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === 'victauri-reconnect' || alarm.name === 'victauri-keepalive') {
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
            const tabs = await api.tabs.query({ active: true, currentWindow: true });
            const activeTab = tabs[0];
            if (!activeTab) {
                sendToHost({ id, type: 'response', error: 'No active tab' });
                return;
            }
            tabId = activeTab.id;
        }

        const tab = await api.tabs.get(tabId);
        if (tab.url && (tab.url.startsWith('about:') || tab.url.startsWith('moz-extension:'))) {
            sendToHost({ id, type: 'response', error: `Cannot inspect ${tab.url} — browser internal pages are not accessible` });
            return;
        }

        if (method === 'screenshot') {
            const data = await captureScreenshot(tabId, args || {});
            sendToHost({ id, type: 'response', data });
        } else if (method === 'getCookies') {
            const tabInfo = await api.tabs.get(tabId);
            const url = tabInfo.url;
            const cookies = await api.cookies.getAll({ url });
            sendToHost({ id, type: 'response', data: cookies });
        } else if (method === 'navigate' && args && args.url) {
            await api.tabs.update(tabId, { url: args.url });
            sendToHost({ id, type: 'response', data: { ok: true, url: args.url } });
        } else if (method === 'navigateBack') {
            await api.tabs.goBack(tabId);
            sendToHost({ id, type: 'response', data: { ok: true } });
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

        api.tabs.sendMessage(
            tabId,
            { type: 'victauri_command', id: commandId, method, args }
        ).then((response) => {
            clearTimeout(timeout);
            if (!response) {
                reject(new Error('No response from content script'));
                return;
            }
            if (response.type === 'error') {
                reject(new Error(response.error));
            } else {
                resolve(response.data);
            }
        }).catch((err) => {
            clearTimeout(timeout);
            reject(err);
        });
    });
}

async function captureScreenshot(tabId, options) {
    // Firefox does not support the chrome.debugger API.
    // Use captureVisibleTab for viewport screenshots.
    const tabs = await api.tabs.query({ active: true, currentWindow: true });
    const activeTab = tabs[0];
    if (activeTab && activeTab.id === tabId) {
        const dataUrl = await api.tabs.captureVisibleTab(null, { format: 'png' });
        return dataUrl.split(',')[1];
    }

    // For non-active tabs or full-page, we need to activate the tab first
    await api.tabs.update(tabId, { active: true });
    // Brief delay to allow tab rendering
    await new Promise(resolve => setTimeout(resolve, 150));
    const dataUrl = await api.tabs.captureVisibleTab(null, { format: 'png' });
    return dataUrl.split(',')[1];
}

// Tab lifecycle tracking
api.tabs.onCreated.addListener((tab) => {
    tabStates.set(tab.id, { url: tab.url || '', title: tab.title || '', bridgeReady: false });
    sendToHost({ type: 'tab_created', tab_id: tab.id, url: tab.url, title: tab.title });
});

api.tabs.onRemoved.addListener((tabId) => {
    tabStates.delete(tabId);
    sendToHost({ type: 'tab_closed', tab_id: tabId });
});

api.tabs.onActivated.addListener(({ tabId }) => {
    sendToHost({ type: 'tab_activated', tab_id: tabId });
});

api.tabs.onUpdated.addListener((tabId, changeInfo) => {
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
api.runtime.onMessage.addListener((message, sender) => {
    if (message.type === 'content_script_ready' && sender.tab) {
        const tabId = sender.tab.id;
        const state = tabStates.get(tabId) || {};
        state.bridgeReady = true;
        tabStates.set(tabId, state);
        sendToHost({ type: 'bridge_ready', tab_id: tabId, url: message.url });
    }
});

// Sync existing tabs on startup
async function syncExistingTabs() {
    try {
        const tabs = await api.tabs.query({});
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
        const activeTabs = await api.tabs.query({ active: true, currentWindow: true });
        if (activeTabs[0]) {
            sendToHost({ type: 'tab_activated', tab_id: activeTabs[0].id });
        }
    } catch (e) {
        console.error('[victauri] Tab sync failed:', e);
    }
}

// Keepalive: Firefox background scripts persist (not service workers),
// but we still use the alarm for reconnection safety.
api.alarms.create('victauri-keepalive', { periodInMinutes: 4 });

// Connect on startup
connectNative();
syncExistingTabs();
