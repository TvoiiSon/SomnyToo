use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub struct InputValidator;

impl InputValidator {
    pub fn validate_ip(ip: IpAddr, allow_private_ips: bool) -> Result<(), String> {
        // Запрещаем специальные IP-адреса всегда
        if ip.is_unspecified() {
            return Err("Unspecified IP address is not allowed".to_string());
        }

        if ip.is_multicast() {
            return Err("Multicast IP address is not allowed".to_string());
        }

        // Loopback разрешаем только в тестовом режиме
        if ip.is_loopback() && !allow_private_ips {
            return Err("Loopback IP address is not allowed".to_string());
        }

        // Private IP разрешаем только если explicitly allowed
        if !allow_private_ips {
            if let IpAddr::V4(ipv4) = ip {
                if Self::is_private_ipv4(ipv4) {
                    return Err("Private IP address is not allowed".to_string());
                }
            }

            if let IpAddr::V6(ipv6) = ip {
                if Self::is_private_ipv6(ipv6) {
                    return Err("Private IPv6 address is not allowed".to_string());
                }
            }
        }

        Ok(())
    }

    pub fn validate_packet_size(size: usize) -> Result<(), String> {
        const MIN_PACKET_SIZE: usize = 1;
        const MAX_PACKET_SIZE: usize = 10 * 1024 * 1024; // 10MB

        if size < MIN_PACKET_SIZE {
            return Err("Packet size too small".to_string());
        }

        if size > MAX_PACKET_SIZE {
            return Err("Packet size too large".to_string());
        }

        Ok(())
    }

    pub fn validate_session_id(session_id: &[u8]) -> Result<(), String> {
        const MIN_SESSION_ID_LENGTH: usize = 8;
        const MAX_SESSION_ID_LENGTH: usize = 64;

        if session_id.len() < MIN_SESSION_ID_LENGTH {
            return Err("Session ID too short".to_string());
        }

        if session_id.len() > MAX_SESSION_ID_LENGTH {
            return Err("Session ID too long".to_string());
        }

        Ok(())
    }

    fn is_private_ipv4(ip: Ipv4Addr) -> bool {
        let octets = ip.octets();

        // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        octets[0] == 10 ||
            (octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31) ||
            (octets[0] == 192 && octets[1] == 168)
    }

    fn is_private_ipv6(ip: Ipv6Addr) -> bool {
        // Unique Local Addresses (fc00::/7)
        let segments = ip.segments();
        segments[0] & 0xfe00 == 0xfc00
    }
}