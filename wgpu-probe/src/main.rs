fn main() {
    env_logger::init();
    let backend = std::env::args().nth(1).unwrap();
    let backends = match backend.as_str() {
        "dx12" => wgpu::Backends::DX12,
        "vulkan" => wgpu::Backends::VULKAN,
        "gl" => wgpu::Backends::GL,
        _ => wgpu::Backends::all(),
    };
    eprintln!("PROBE: creating instance {backends:?}");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    eprintln!("PROBE: requesting adapter");
    let adapter = spin_on::spin_on(instance.request_adapter(&Default::default()));
    eprintln!("PROBE: adapter result {adapter:?}");
    if let Ok(adapter) = adapter {
        eprintln!("PROBE: adapter info {:?}", adapter.get_info());
        eprintln!("PROBE: requesting device");
        let device = spin_on::spin_on(adapter.request_device(&Default::default()));
        eprintln!("PROBE: device result {device:?}");
    }
    eprintln!("PROBE: done");
}
