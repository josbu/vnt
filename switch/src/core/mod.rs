use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use crossbeam::atomic::AtomicCell;
use crossbeam_skiplist::SkipMap;
use parking_lot::Mutex;
use p2p_channel::boot::Boot;
use p2p_channel::channel::{Channel, Route, RouteKey};
use p2p_channel::punch::NatInfo;
use crate::handle::{ConnectStatus, CurrentDeviceInfo, heartbeat_handler, PeerDeviceInfo, punch_handler, recv_handler, registration_handler, tap_handler, tun_handler};
use crate::nat::NatTest;
use crate::{tap_device, tun_device};
use crate::tap_device::TapWriter;
use crate::tun_device::TunWriter;

pub struct Switch {
    name: String,
    current_device: Arc<AtomicCell<CurrentDeviceInfo>>,
    tun_writer: Option<TunWriter>,
    tap_writer: Option<TapWriter>,
    nat_channel: Channel<Ipv4Addr>,
    /// 0. 机器纪元，每一次上线或者下线都会增1，用于感知网络中机器变化
    /// 服务端和客户端的不一致，则服务端会推送新的设备列表
    /// 1. 网络中的虚拟ip列表
    device_list: Arc<Mutex<(u16, Vec<PeerDeviceInfo>)>>,
    nat_test: NatTest,
    connect_status: Arc<AtomicCell<ConnectStatus>>,
    peer_nat_info_map: Arc<SkipMap<Ipv4Addr, NatInfo>>,
}

impl Switch {
    pub fn start(config: Config) -> crate::Result<Switch> {
        log::info!("config:{:?}",config);
        let (mut channel, punch, idle) = Boot::new::<Ipv4Addr>(80, 15000, 0)?;
        let response = registration_handler::registration(&mut channel, config.server_address, config.token.clone(), config.device_id.clone(), config.name.clone())?;
        let register = Arc::new(registration_handler::Register::new(channel.sender()?, config.server_address, config.token.clone(), config.device_id.clone(), config.name.clone()));
        let device_list: Arc<Mutex<(u16, Vec<PeerDeviceInfo>)>> = Arc::new(Mutex::new((0, Vec::new())));
        let peer_nat_info_map: Arc<SkipMap<Ipv4Addr, NatInfo>> = Arc::new(SkipMap::new());
        let connect_status = Arc::new(AtomicCell::new(ConnectStatus::Connected));
        let virtual_ip = Ipv4Addr::from(response.virtual_ip);
        let virtual_gateway = Ipv4Addr::from(response.virtual_gateway);
        let virtual_netmask = Ipv4Addr::from(response.virtual_netmask);

        let local_ip = crate::nat::local_ip()?;
        let local_port = channel.local_addr()?.port();
        // NAT检测
        let nat_test = NatTest::new(config.nat_test_server.clone(), Ipv4Addr::from(response.public_ip), response.public_port as u16, local_ip, local_port);
        let (current_device, tun_writer, tap_writer) = if config.tap {
            #[cfg(windows)]
            {
                //删除switch的tun网卡避免ip冲突，因为非正常退出会保留网卡
                tun_device::delete_tun();
            }
            let (tap_writer, tap_reader, mac) = tap_device::create_tap(virtual_ip, virtual_netmask, virtual_gateway)?;
            let current_device = Arc::new(AtomicCell::new(CurrentDeviceInfo::new(virtual_ip, virtual_gateway, virtual_netmask,
                                                                                 config.server_address, mac)));
            //tap数据处理
            tap_handler::start(channel.sender()?, tap_reader.clone(), tap_writer.clone(), current_device.clone());
            (current_device, None, Some(tap_writer))
        } else {
            #[cfg(windows)]
            {
                //删除switch的tap网卡避免ip冲突，非正常退出会保留网卡
                tap_device::delete_tap();
            }
            // tun通道
            let (tun_writer, tun_reader) = tun_device::create_tun(virtual_ip, virtual_netmask, virtual_gateway)?;
            let current_device = Arc::new(AtomicCell::new(CurrentDeviceInfo::new(virtual_ip, virtual_gateway, virtual_netmask, config.server_address, [0, 0, 0, 0, 0, 0])));
            //tun数据接收处理
            tun_handler::start(channel.sender()?, tun_reader.clone(), tun_writer.clone(), current_device.clone());
            (current_device, Some(tun_writer), None)
        };
        //外部数据接收处理
        let channel_recv_handler = recv_handler::RecvHandler::new(channel.try_clone()?, current_device.clone(), device_list.clone(), register.clone(),
                                                                  nat_test.clone(), tun_writer.clone(), tap_writer.clone(), connect_status.clone(), peer_nat_info_map.clone());
        recv_handler::start(channel_recv_handler);
        // 定时心跳
        heartbeat_handler::start_heartbeat(channel.sender()?, device_list.clone(), current_device.clone());
        // 空闲检查
        heartbeat_handler::start_idle(idle, channel.sender()?);
        // 打洞处理
        punch_handler::start_cone(punch.try_clone()?, current_device.clone());
        punch_handler::start_symmetric(punch, current_device.clone());
        punch_handler::start_punch(nat_test.clone(), device_list.clone(), channel.sender()?, current_device.clone());
        log::info!("switch启动成功");
        Ok(Switch {
            name: config.name,
            current_device,
            tun_writer,
            tap_writer,
            nat_channel: channel,
            nat_test,
            device_list,
            connect_status,
            peer_nat_info_map,
        })
    }
}

impl Switch {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn current_device(&self) -> CurrentDeviceInfo {
        self.current_device.load()
    }
    pub fn peer_nat_info(&self, ip: &Ipv4Addr) -> Option<NatInfo> {
        self.peer_nat_info_map.get(ip).map(|e| e.value().clone())
    }
    pub fn connection_status(&self) -> ConnectStatus {
        self.connect_status.load()
    }
    pub fn nat_info(&self) -> NatInfo {
        self.nat_test.nat_info()
    }
    pub fn device_list(&self) -> Vec<PeerDeviceInfo> {
        let device_list_lock = self.device_list.lock();
        let (_epoch, device_list) = device_list_lock.clone();
        drop(device_list_lock);
        device_list
    }
    pub fn route(&self, ip: &Ipv4Addr) -> Option<Route> {
        self.nat_channel.route(ip)
    }
    pub fn route_key(&self, route_key: &RouteKey) -> Option<Ipv4Addr> {
        self.nat_channel.route_to_id(route_key)
    }
    pub fn route_table(&self) -> Vec<(Ipv4Addr, Route)> {
        self.nat_channel.route_table()
    }
    pub fn stop(&self) -> io::Result<()> {
        if let Some(tap) = &self.tap_writer {
            tap.close()?;
        }
        if let Some(tun) = &self.tun_writer {
            tun.close()?;
        }
        self.nat_channel.close()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub tap: bool,
    pub token: String,
    pub device_id: String,
    pub name: String,
    pub server_address: SocketAddr,
    pub nat_test_server: Vec<SocketAddr>,
}

impl Config {
    pub fn new(tap: bool, token: String,
               device_id: String,
               name: String,
               server_address: SocketAddr,
               nat_test_server: Vec<SocketAddr>, ) -> Self {
        Self {
            tap,
            token,
            device_id,
            name,
            server_address,
            nat_test_server,
        }
    }
}