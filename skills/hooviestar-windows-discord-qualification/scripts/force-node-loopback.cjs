'use strict';

const net = require('node:net');
const originalListen = net.Server.prototype.listen;

net.Server.prototype.listen = function listenLoopback(...arguments_) {
    const numericPort = typeof arguments_[0] === 'number' ||
        (typeof arguments_[0] === 'string' && /^\d+$/.test(arguments_[0]));
    if (numericPort) {
        if (typeof arguments_[1] === 'string' || arguments_[1] === null) {
            arguments_[1] = '127.0.0.1';
        } else {
            arguments_.splice(1, 0, '127.0.0.1');
        }
    } else if (
        arguments_[0] &&
        typeof arguments_[0] === 'object' &&
        'port' in arguments_[0]
    ) {
        arguments_[0] = { ...arguments_[0], host: '127.0.0.1' };
    }
    return Reflect.apply(originalListen, this, arguments_);
};
