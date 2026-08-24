#version 450

// Scene-item vertex stage: one static quad drawn via gl_VertexIndex (no per-frame
// allocations). Positions are item pixels in output space, rotated about the item
// center and mapped to Vulkan NDC (+y down, matching top-down framebuffer rows).
layout(push_constant) uniform Push {
    vec2 center;      // item center in output pixels
    vec2 halfExtent;  // half width / height in output pixels
    vec2 cosSin;      // rotation about center
    vec2 uvScale;
    vec2 uvOffset;
    vec2 outputSize;
    float opacity;
    uint mode;        // 0 = sampled texture, 1 = unavailable panel
} pc;

layout(location = 0) out vec2 vUv;

void main() {
    vec2 corner = vec2(
        float((gl_VertexIndex & 1u) != 0u),
        float((gl_VertexIndex & 2u) != 0u)
    ); // (0,0), (1,0), (0,1), (1,1)

    vec2 local = (corner - 0.5) * pc.halfExtent * 2.0;
    mat2 rot = mat2(pc.cosSin.x, -pc.cosSin.y, pc.cosSin.y, pc.cosSin.x);
    vec2 position = pc.center + rot * local;

    gl_Position = vec4(
        position.x / pc.outputSize.x * 2.0 - 1.0,
        position.y / pc.outputSize.y * 2.0 - 1.0,
        0.0,
        1.0
    );
    vUv = pc.uvOffset + corner * pc.uvScale;
}
