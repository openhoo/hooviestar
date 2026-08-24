#version 450

layout(push_constant) uniform Push {
    vec2 center;
    vec2 halfExtent;
    vec2 cosSin;
    vec2 uvScale;
    vec2 uvOffset;
    vec2 outputSize;
    float opacity;
    uint mode;
} pc;

layout(binding = 0) uniform sampler2D sourceTexture;
layout(location = 0) in vec2 vUv;
layout(location = 0) out vec4 outColor;

void main() {
    if (pc.mode == 1u) {
        vec2 sizePx = max(pc.halfExtent * 2.0, vec2(1.0));
        vec2 border = vec2(3.0) / sizePx;
        bool edge = vUv.x < border.x || vUv.y < border.y ||
                    vUv.x > 1.0 - border.x || vUv.y > 1.0 - border.y;
        float stripe = step(0.5, fract((gl_FragCoord.x - gl_FragCoord.y) / 28.0));
        vec3 color = mix(vec3(0.125, 0.153, 0.212),
                         vec3(0.169, 0.208, 0.298), stripe);
        if (edge) color = vec3(0.231, 0.290, 0.420);
        outColor = vec4(color * pc.opacity, pc.opacity);
        return;
    }

    vec4 color = texture(sourceTexture, vUv);
    outColor = vec4(color.rgb * color.a * pc.opacity, color.a * pc.opacity);
}
