struct Particle { 
    float2 position;
    float2 velocity;
    float4 color;
}

struct PushConstants { 
    uint output_image;
    uint particles;
    uint present_texture;
    uint present_sampler;
    uint2 window;
    float2 mouse;
    float delta_time;
    float pad0;
}

[[vk::binding(1, 0)]]
RWStructuredBuffer<Particle> particle_buffers[];

[[vk::binding(2, 0)]]
Texture2D<float4> sampled_images[];

[[vk::binding(3, 0)]]
RWTexture2D<unorm float4> storage_images[];

[[vk::binding(4, 0)]]
SamplerState samplers[];

[[vk::push_constant]]
cbuffer PC {
    PushConstants pc;
};

[shader("compute")]
[numthreads(16, 16, 1)]
void computeClear(uint3 global_id : SV_DispatchThreadID) {
    if (global_id.x >= pc.window.x || global_id.y >= pc.window.y)
        return;

    int2 coord = int2(global_id.xy);

    float4 current = storage_images[pc.output_image][coord];

    float  fade_speed = 0.95;
    float4 faded      = float4(current.rgb * fade_speed, current.a);

    storage_images[pc.output_image][coord] = faded;
}

[shader("compute")]
[numthreads(256, 1, 1)]
void computeMain(uint3 global_id : SV_DispatchThreadID)
{
    uint idx = global_id.x;

    Particle particle = particle_buffers[pc.particles][idx];

    particle.position += particle.velocity * pc.delta_time;

    if (particle.position.x <= 0.0 || particle.position.x >= float(pc.window.x)) {
        particle.velocity.x = -particle.velocity.x;
    }
    if (particle.position.y <= 0.0 || particle.position.y >= float(pc.window.y)) {
        particle.velocity.y = -particle.velocity.y;
    }

    float2 mouse_pos = pc.mouse;
    float2 to_mouse  = mouse_pos - particle.position;
    float  dist      = length(to_mouse);

    float min_dist = -0.5;
    if (dist > min_dist) {
        float2 force = normalize(to_mouse) * 800.0 / dist;
        particle.velocity += force * pc.delta_time;
    } else {
        float2 repel = normalize(-to_mouse) * 400.0;
        particle.velocity += repel * pc.delta_time;
    }

    particle.velocity *= 0.995;

    float max_speed     = 400.0;
    float current_speed = length(particle.velocity);
    if (current_speed > max_speed) {
        particle.velocity = normalize(particle.velocity) * max_speed;
    }

    particle_buffers[pc.particles][idx] = particle;

    int2 pos = int2(particle.position);

    if (pos.x >= 0 && pos.x < int(pc.window.x) &&
        pos.y >= 0 && pos.y < int(pc.window.y))
    {
        float4 current  = storage_images[pc.output_image][pos];
        float4 blended  = max(current, particle.color);
        storage_images[pc.output_image][pos] = blended;
    }
}

struct VertexOutput {
    float4 pos : SV_Position;
    float2 uv;
};

[shader("vertex")]
VertexOutput vertexMain(uint vert_idx : SV_VertexID)
{
    static const float2 positions[6] = {
        float2(-1.0, -1.0),  // bottom-left
        float2(-1.0,  1.0),  // top-left
        float2( 1.0, -1.0),  // bottom-right

        float2(-1.0,  1.0),  // top-left
        float2( 1.0,  1.0),  // top-right
        float2( 1.0, -1.0)   // bottom-right
    };

    static const float2 uvs[6] = {
        float2(0.0, 1.0),  // bottom-left
        float2(0.0, 0.0),  // top-left
        float2(1.0, 1.0),  // bottom-right

        float2(0.0, 0.0),  // top-left
        float2(1.0, 0.0),  // top-right
        float2(1.0, 1.0)   // bottom-right
    };

    VertexOutput output;
    output.pos = float4(positions[vert_idx], 0.0, 1.0);
    output.uv  = uvs[vert_idx];
    return output;
}


[shader("fragment")]
float4 fragmentMain(float2 uv : TEXCOORD0) : SV_Target0 {
    return sampled_images[pc.present_texture].Sample(samplers[pc.present_sampler], uv);
}
