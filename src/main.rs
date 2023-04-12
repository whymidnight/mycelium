use std::{
    error::Error, 
    net::{Ipv4Addr},
};

use clap::Parser;
use tokio::sync::mpsc;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use etherparse::{IpHeader, icmpv6, Icmpv6Header, ip_number, Icmpv4Header, Icmpv4Type};
use etherparse::PacketHeaders;


mod node_setup;
mod peer;
mod peer_manager;
mod packet_control;

use peer::Peer;
use peer_manager::PeerManager;

#[derive(Parser)]
struct Cli {
    #[arg(short = 'a', long = "tun-addr")]
    tun_addr: Ipv4Addr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    
    let cli = Cli::parse();

    // Create TUN interface and add static route
    let node_tun = match node_setup::setup_node(cli.tun_addr).await {
        Ok(tun)=> {
            println!("Node setup complete");
            tun
        },
        Err(e) => {
            panic!("Error setting up node: {}", e);
        }
    };

    // Create an unbounded channel for this node
    let (to_tun, mut from_peers) = mpsc::unbounded_channel::<Vec<u8>>();

    // Create the PeerManager: an interface to all peers this node is connected to
    // Each node should include itself as a peer
    // Additional static peers are obtained through the nodeconfig.toml file
    let myself = Peer{id: "0".to_string(), to_peer: to_tun.clone()}; 
    let peer_manager = PeerManager::new(myself);

    let peer_man_clone = peer_manager.clone();
    tokio::spawn(async move {
        peer_man_clone.get_peers_from_config(to_tun.clone()).await; // --> here we create peer by TcpStream connect

        // listen for inbound request --> "to created the reverse peer object" --> here we reverse create peer be listener.accept'ing
        tokio::spawn(async move {
            match TcpListener::bind("[::]:9651").await {
                Ok(listener) => {
                    // loop to accept the inbound requests
                    loop {
                        match listener.accept().await {
                            Ok((stream, _)) => {
                                println!("Got inbound request from: {}", stream.peer_addr().unwrap().to_string());
                                // "reverse peer add"
                                let peer_id = stream.peer_addr().unwrap().to_string();
                                match Peer::new(peer_id, to_tun.clone(), stream) {
                                    Ok(new_peer) => {
                                        peer_man_clone.known_peers.lock().unwrap().push(new_peer);
                                    },
                                    Err(e) => {
                                        eprintln!("Error creating 'reverse' peer: {}", e);
                                    }
                                }
                            },
                            Err(e) => {
                                eprintln!("Error accepting TCP listener: {}", e);
                            }
                       }
                    }
                }, 
                Err(e) => {
                    eprintln!("Error binding TCP listener: {}", e);
                }
            }
        })
    });

    // Loop to read the 'from_peers' receiver and foward it toward the TUN interface
    let node_tun_clone = node_tun.clone();
    tokio::spawn(async move{
        loop {
            while let Some(packet) = from_peers.recv().await {
                match node_tun_clone.send(&packet).await {
                    Ok(packet) => {
                        //println!("Received from 'from_peers': {:?}", packet);
                    },
                    Err(e) => {
                        eprintln!("Error sending to TUN interface: {}", e);
                    }
                }
            }
        }
    });

    // TODO: Loop to read from the TUN interface 
    // ??? to send: and forward it towards to correct destination peer (by selecting the correct to_peer sender)
    let node_tun_clone = node_tun.clone();

    let peer_man_clone = peer_manager.clone();


    tokio::spawn(async move{
        loop {
            let link_mtu = 1500;
            let mut buf = vec![0u8; link_mtu];
            match node_tun_clone.recv(&mut buf).await{
                Ok(n) => {

                    buf.truncate(n);
                    

                    // TEMPORARY
                    // To play with etherparse, let create some basic filters for
                    // ICMP and ICMPv6 packets. Note: these have no real purpose other than
                    // playing around with etherparse crate

                    let (header, _, remainder) = IpHeader::from_slice(&buf).unwrap();

                    // Differentiate between IPv4 and IPv6 packets
                    match header {
                        IpHeader::Version4(ipv4_header, _) => {
                            // ICMP packets
                            if ipv4_header.protocol == ip_number::ICMP {
                               let icmp_header = Icmpv4Header::from_slice(&remainder).unwrap().0;
                                match icmp_header.icmp_type {
                                    Icmpv4Type::EchoRequest(_) => println!("ICMP Echo Request packet"),
                                    Icmpv4Type::EchoReply(_) => println!("ICMP Echo Reply packet"),
                                    // ... additional ICMP packets type belows
                                    _ => (),
                                }
                            }
                        },
                        IpHeader::Version6(ipv6_header, _) => {
                            // ICMP packets
                            if ipv6_header.next_header == ip_number::IPV6_ICMP {
                                let icmpv6_header = Icmpv6Header::from_slice(&remainder).unwrap().0;
                                match icmpv6_header.icmp_type.type_u8() {
                                    icmpv6::TYPE_ROUTER_SOLICITATION => println!("ICMPv6 Router Solicitation packet"),
                                    icmpv6::TYPE_ROUTER_ADVERTISEMENT => println!("ICMPv6 Router Advertisement packet"),
                                    // ... additional ICMPv6 packet types below
                                    _ => (),
                                }
                            }
                        }
                    }


                    // TEMPORARY: send the message to the Node B (looksaus) --> caution: normally
                    // we need to check based upon some kind of ID where to send the packet to
                    // by selecting the correct to_peer sender halve
                    if let Some(first_peer) = &peer_man_clone.known_peers.lock().unwrap().get(1) {
                        first_peer.to_peer.send(buf).unwrap();
                    }



                },
                Err(e) => {
                    eprintln!("Error reading from TUN interface: {}", e);
                }
            }
            // WE NEED TO WORK WITH FRAMED HERE

            
            

        }
    });


    tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24)).await;
    Ok(())
}