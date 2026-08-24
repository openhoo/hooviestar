#version 450

layout(location = 0) out vec2 vUv;

void main() {
    // Fullscreen triangle: vertices (0,0), (2,0), (0,2).
    vec2 position = vec2(float((gl_VertexIndex << 1) & 2),
                         float(gl_VertexIndex & 2u));
    gl_Position = vec4(position * 2.0 - 1.0, 0.0, 1.0);
    vUv = position;
}
