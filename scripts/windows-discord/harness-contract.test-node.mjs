import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import net from 'node:net';
import { once } from 'node:events';
import test from 'node:test';

const read = (name) => readFile(new URL(name, import.meta.url), 'utf8');

test('publisher uses release binaries and keeps occluded browser video active', async () => {
    const publisher = await read('./Start-Publisher.ps1');
    assert.match(publisher, /cargo build --release -p hooviestar-engine/);
    assert.match(publisher, /target\\release\\examples\\qualify_windows_pipeline\.exe/);
    assert.match(publisher, /--disable-backgrounding-occluded-windows/);
    assert.match(publisher, /--disable-features=CalculateNativeWinOcclusion/);
    assert.match(publisher, /--program-topmost-gate/);
});

test('MiroTalk clients restart an isolated process tree and prove IPv4 CDP readiness', async () => {
    const client = await read('./Start-MiroTalkClient.ps1');
    assert.match(client, /Stop-QualificationProcessTree/);
    assert.match(client, /CommandLine\.Contains\(\$profile\)/);
    assert.match(client, /Get-NetTCPConnection -State Listen -LocalPort \$CdpPort/);
    assert.match(client, /http:\/\/127\.0\.0\.1:\$CdpPort\/json\/version/);
    assert.match(client, /--remote-debugging-address=127\.0\.0\.1/);
});

test('window mapper restores minimized Program before checking exact geometry', async () => {
    const mapper = await read('./Move-QualificationProgramOnscreen.ps1');
    assert.match(mapper, /IsIconic/);
    assert.match(mapper, /ShowWindowAsync\(\$window, 9\)/);
    assert.match(mapper, /expected 1280x720 onscreen rectangle/);
});

test('task wrappers reject stale or cross-run qualification reports', async () => {
    for (const name of [
        'Invoke-NativeQualificationTask.ps1',
        'Invoke-MiroTalkPublisherTask.ps1',
        'Invoke-MiroTalkReceiverTask.ps1',
        'Invoke-DiscordPublisherTask.ps1',
        'Invoke-DiscordReceiverTask.ps1',
    ]) {
        const wrapper = await read(`./${name}`);
        assert.match(wrapper, /reportFresh/);
        assert.match(wrapper, /reportRunId/);
        assert.match(wrapper, /qualificationRunId/);
    }
});

test('MiroTalk preload overrides positional and options wildcard listeners', async () => {
    const require = createRequire(import.meta.url);
    require('../../skills/hooviestar-windows-discord-qualification/scripts/force-node-loopback.cjs');
    for (const listen of [
        (server) => server.listen(0, '0.0.0.0'),
        (server) => server.listen({ port: 0, host: '::' }),
    ]) {
        const server = net.createServer();
        listen(server);
        await once(server, 'listening');
        assert.equal(server.address().address, '127.0.0.1');
        server.close();
        await once(server, 'close');
    }
});

test('versioned qualification skill resolves its repository instructions', async () => {
    const skill = new URL('../../skills/hooviestar-windows-discord-qualification/SKILL.md', import.meta.url);
    const skillText = await readFile(skill, 'utf8');
    assert.match(skillText, /\.\.\/\.\.\/docs\/windows-discord-qualification\.md/);
    await readFile(new URL('../../docs/windows-discord-qualification.md', skill), 'utf8');
    await readFile(new URL('../../scripts/windows-discord/Start-Publisher.ps1', skill), 'utf8');
});
