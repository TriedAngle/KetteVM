struct Instance {
    float2 pos;
    float2 size;
    float4 color;
}

struct PushData {
    uint instances;
    float screen_width;
    float screen_height;
    float pad;
}

[[vk::binding(0, 0)]]
StructuredBuffer<Instance> instance_buffers[];

[[vk::push_constant]]
cbuffer PC {
    PushData pc;
}

struct VertexOutput {
    float4 position : SV_Position;
    float4 color;
}

static const float2 corners[6] = {
    float2(0.0, 0.0),
    float2(1.0, 0.0),
    float2(0.0, 1.0),
    float2(0.0, 1.0),
    float2(1.0, 0.0),
    float2(1.0, 1.0),
};

[shader("vertex")]
VertexOutput vertexMain(uint vertex_id : SV_VertexID, uint instance_id : SV_InstanceID) {
    Instance instance = instance_buffers[pc.instances][instance_id];
    float2 p = instance.pos + corners[vertex_id] * instance.size;
    float2 ndc = float2(
        p.x / pc.screen_width * 2.0 - 1.0,
        p.y / pc.screen_height * 2.0 - 1.0
    );

    VertexOutput output;
    output.position = float4(ndc, 0.0, 1.0);
    output.color = instance.color;
    return output;
}

[shader("fragment")]
float4 fragmentMain(VertexOutput input) : SV_Target0 {
    return input.color;
}
