#!/usr/bin/env node

const [command, portText] = process.argv.slice(2);
const port = Number(portText);
if (!['publisher', 'receiver', 'receiver-muted', 'stop', 'status'].includes(command) || !Number.isInteger(port)) {
    console.error(
        'usage: node Control-MiroTalkCdp.mjs <publisher|receiver|receiver-muted|stop|status> <port>',
    );
    process.exit(2);
}

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => response.json());
const target = targets.find((entry) => entry.type === 'page' && entry.url.includes('127.0.0.1:3016'));
if (!target) throw new Error(`No MiroTalk page target on CDP port ${port}`);

const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true });
    socket.addEventListener('error', reject, { once: true });
});
let sequence = 0;
const pending = new Map();
socket.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (!message.id || !pending.has(message.id)) return;
    const { resolve, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(message.error.message));
    else resolve(message.result);
});

function send(method, params = {}) {
    const id = ++sequence;
    socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}

let receiverContextId;
let receiverFrameUrl;
if (target.url.includes('/viewer')) {
    const frameTree = await send('Page.getFrameTree');
    const findReceiverFrame = (tree) => {
        if (tree.frame.url.includes('/viewer')) return tree.frame;
        for (const child of tree.childFrames ?? []) {
            const match = findReceiverFrame(child);
            if (match) return match;
        }
        return undefined;
    };
    const receiverFrame = findReceiverFrame(frameTree.frameTree) ?? frameTree.frameTree.frame;
    receiverFrameUrl = receiverFrame.url;
    const world = await send('Page.createIsolatedWorld', {
        frameId: receiverFrame.id,
        worldName: 'hooviestar-receiver-control',
        grantUniveralAccess: true,
    });
    receiverContextId = world.executionContextId;
}

async function evaluate(expression, userGesture = false) {
    const result = await send('Runtime.evaluate', {
        expression,
        awaitPromise: true,
        returnByValue: true,
        userGesture,
        ...(receiverContextId ? { contextId: receiverContextId } : {}),
    });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
    return result.result.value;
}

async function click(selector) {
    await send('Page.bringToFront');
    const box = await evaluate(`(() => {
        const element = document.querySelector(${JSON.stringify(selector)});
        if (!element) throw new Error('Missing element: ${selector}');
        const rectangle = element.getBoundingClientRect();
        return { x: rectangle.left + rectangle.width / 2, y: rectangle.top + rectangle.height / 2 };
    })()`);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: box.x, y: box.y });
    await send('Input.dispatchMouseEvent', {
        type: 'mousePressed',
        x: box.x,
        y: box.y,
        button: 'left',
        buttons: 1,
        clickCount: 1,
    });
    await send('Input.dispatchMouseEvent', {
        type: 'mouseReleased',
        x: box.x,
        y: box.y,
        button: 'left',
        buttons: 0,
        clickCount: 1,
    });
}

if (command === 'publisher') {
    await evaluate(`(() => {
        document.title = 'Hooviestar MiroTalk Publisher';
        window.__hooviestarClickCount = 0;
        window.__hooviestarErrors = [];
        document.querySelector('#screenShareStart').addEventListener('click', () => window.__hooviestarClickCount++);
        const originalError = console.error.bind(console);
        console.error = (...values) => {
            window.__hooviestarErrors.push(values.map((value) => String(value)).join(' '));
            originalError(...values);
        };
        document.querySelector('.swal2-confirm')?.click();
    })()`);
    await evaluate(`document.querySelector('#screenShareStart').click()`, true);
    await delay(3_000);
} else if (command === 'receiver') {
    await evaluate(`document.title = 'Hooviestar MiroTalk Receiver'`);
    await click('#mainVideo');
    await evaluate(`(() => {
        const video = document.querySelector('#mainVideo');
        video.muted = false;
        video.volume = 1;
        return video.play().catch(() => undefined);
    })()`);
    await delay(3_000);
} else if (command === 'receiver-muted') {
    await evaluate(`(() => {
        document.title = 'Hooviestar MiroTalk Receiver';
        const video = document.querySelector('#mainVideo');
        video.muted = true;
        video.volume = 0;
        return video.play().catch(() => undefined);
    })()`);
    await delay(1_000);
} else if (command === 'stop') {
    await evaluate(`document.querySelector('#screenShareStop')?.click()`, true);
    await delay(1_000);
}

const status = await evaluate(`(() => {
    const selector = location.pathname.includes('broadcast') ? 'video' : '#mainVideo';
    const video = document.querySelector(selector);
    const stream = video?.srcObject;
    const describe = (track) => ({
        kind: track.kind,
        label: track.label,
        enabled: track.enabled,
        muted: track.muted,
        readyState: track.readyState,
        settings: track.getSettings(),
    });
    return {
        title: document.title,
        url: location.href,
        documentReadyState: document.readyState,
        applicationLoaded: typeof toggleScreen === 'function',
        screenShareEnabled: typeof screenShareEnabled === 'boolean' ? screenShareEnabled : null,
        screenButtonVisible: Boolean(document.querySelector('#screenShareStart')?.offsetParent),
        screenButtonBox: document.querySelector('#screenShareStart')?.getBoundingClientRect().toJSON() ?? null,
        qualificationClickCount: window.__hooviestarClickCount ?? null,
        qualificationErrors: window.__hooviestarErrors ?? [],
        readyState: video?.readyState ?? -1,
        paused: video?.paused ?? true,
        muted: video?.muted ?? true,
        width: video?.videoWidth ?? 0,
        height: video?.videoHeight ?? 0,
        tracks: stream ? stream.getTracks().map(describe) : [],
    };
})()`);
status.cdpTargetUrl = target.url;
if (receiverFrameUrl) status.cdpFrameUrl = receiverFrameUrl;
socket.close();
console.log(JSON.stringify(status, null, 2));
