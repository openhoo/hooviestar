#version 450

layout(binding = 0) uniform sampler2D sceneTexture;
layout(location = 0) in vec2 vUv;
layout(location = 0) out vec4 outColor;

void main() {
    // Scene content is already SDR and premultiplied. Clamp is the final
    // bounded SDR pass; no CPU readback or staging copy is involved.
    outColor = vec4(clamp(texture(sceneTexture, vUv).rgb, 0.0, 1.0), 1.0);
}
