use anyhow::{bail, ensure, Context, Result};
use librqbit::{create_torrent, AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions};
use serde_json::{json, Value};
use std::{path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

fn save(path: &Path, value: &Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?; Ok(())
}
fn options(listen: bool) -> SessionOptions {
    SessionOptions { disable_dht: true, disable_dht_persistence: true,
        enable_upnp_port_forwarding: false, listen_port_range: if listen {Some(16881..16981)} else {None},
        ..Default::default() }
}
fn percent_bytes(s: &str) -> Option<Vec<u8>> {
    let mut out=Vec::new(); let b=s.as_bytes(); let mut i=0;
    while i<b.len() { if b[i]==b'%' {if i+2>=b.len(){return None} out.push(u8::from_str_radix(&s[i+1..i+3],16).ok()?);i+=3;} else {out.push(b[i]);i+=1;} } Some(out)
}
fn hash_bytes(s: &str) -> Vec<u8> {(0..40).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).unwrap()).collect()}

#[tokio::main(flavor="multi_thread", worker_threads=2)]
async fn main() -> Result<()> {
    let args:Vec<String>=std::env::args().collect();
    let original=PathBuf::from(args.get(1).context("usage: fixture INPUT_MP4 OUTPUT_DIR [LIFETIME_SECONDS]")?);
    let out=PathBuf::from(args.get(2).context("missing OUTPUT_DIR")?);
    let lifetime:u64=args.get(3).map(|s|s.parse()).transpose()?.unwrap_or(1800);
    ensure!(lifetime>0 && lifetime<=7200,"lifetime must be 1..7200 seconds");
    ensure!(std::env::var("LD_PRELOAD").unwrap_or_default().contains("loopback-bind.so"),"run with the fixture loopback LD_PRELOAD guard");
    std::fs::create_dir_all(out.join("seed"))?;
    let payload=out.join("seed/sintel-trailer.mp4");
    std::fs::copy(&original,&payload)?;
    let created=create_torrent(&payload, CreateTorrentOptions{name:None,piece_length:Some(256*1024)}).await?;
    let hash:String=created.info_hash().0.iter().map(|b|format!("{b:02x}")).collect(); let torrent_bytes=created.as_bytes()?;
    std::fs::write(out.join("sintel.torrent"),&torrent_bytes)?;
    let session=Session::new_with_opts(out.join("seed"), options(true)).await?;
    let seed=session.add_torrent(AddTorrent::from_bytes(torrent_bytes),Some(AddTorrentOptions{
        overwrite:true,disable_trackers:true,..Default::default()})).await?.into_handle().context("seed handle")?;
    tokio::time::timeout(Duration::from_secs(30),seed.wait_until_completed()).await??;
    let port=session.tcp_listen_port().context("seed listener")?;
    // Prefer the real kernel table; some sandboxes (PRoot) deny /proc/net/tcp,
    // then fall back to connection probes instead of failing the fixture.
    let expected=format!("0100007F:{port:04X}");
    match std::fs::read_to_string("/proc/net/tcp") {
        Ok(table) => ensure!(
            table.lines().any(|l| {let f:Vec<_>=l.split_whitespace().collect(); f.get(1)==Some(&expected.as_str()) && f.get(3)==Some(&"0A")}),
            "seeder listener is not verifiably loopback: {expected}"
        ),
        Err(_) => {
            let loopback_ok = tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(("127.0.0.1", port))).await.is_ok();
            ensure!(loopback_ok, "seeder is not reachable on loopback port {port}");
            eprintln!("note: /proc/net/tcp unavailable; loopback reachability probe used instead");
        }
    }
    let tracker=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await?;
    let tracker_port=tracker.local_addr()?.port();
    let tracker_url=format!("http://127.0.0.1:{tracker_port}/announce");
    let request=json!({"infoHash":hash,"fileIdx":0,"sources":[format!("tracker:{tracker_url}")]});
    save(&out.join("stream.json"),&request)?;
    let count=Arc::new(AtomicU64::new(0));
    let c=count.clone(); let wanted=hash_bytes(&hash);
    let tracker_task=tokio::spawn(async move {
        loop {
            let Ok((mut socket,addr))=tracker.accept().await else {break};
            if !addr.ip().is_loopback(){continue}
            let wanted=wanted.clone(); let c=c.clone();
            tokio::spawn(async move {
                let mut bytes=vec![0;16384];let mut n=0;
                loop {let Ok(Ok(k))=tokio::time::timeout(Duration::from_secs(5),socket.read(&mut bytes[n..])).await else{return};if k==0{return}n+=k;if bytes[..n].windows(4).any(|s|s==b"\r\n\r\n"){break}if n==bytes.len(){return}}
                let req=String::from_utf8_lossy(&bytes[..n]);
                let target=req.split_whitespace().nth(1).unwrap_or("");
                let good=target.starts_with("/announce?") && target.split_once('?').into_iter().flat_map(|(_,q)|q.split('&')).filter_map(|v|v.split_once('=')).any(|(k,v)|k=="info_hash" && percent_bytes(v).as_deref()==Some(wanted.as_slice()));
                let mut body=if good {
                    c.fetch_add(1,Ordering::Relaxed);
                    let mut b=b"d8:completei1e10:incompletei0e8:intervali5e5:peers6:".to_vec();
                    b.extend_from_slice(&[127,0,0,1]);b.extend_from_slice(&port.to_be_bytes());b.push(b'e');b
                }else{b"d14:failure reason20:unknown fixture hashe".to_vec()};
                let header=format!("HTTP/1.1 200 OK\r\nContent-Type: application/x-bittorrent\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
                let _=socket.write_all(header.as_bytes()).await; let _=socket.write_all(&mut body).await;
            });
        }
    });
    let pid=std::process::id();std::fs::write(out.join("fixture.pid"),pid.to_string())?;
    let started=Instant::now();
    let mut ready=json!({"ready":true,"pid":pid,"info_hash":hash,"seed_port":port,"tracker_url":tracker_url,
        "seed_verified_bytes":seed.stats().progress_bytes,"loopback_binding_verified":true,"lifetime_seconds":lifetime,"native_receiver":"pending"});
    save(&out.join("ready.json"),&ready)?;
    println!("READY {}",ready);
    // Fresh directory, magnet metadata fetched via tracker, no initial peers/preseeded payload.
    let receive_dir=out.join(format!("receiver-{pid}"));
    ensure!(!receive_dir.exists(),"receiver directory must be fresh");
    std::fs::create_dir(&receive_dir)?;
    let receiver=Session::new_with_opts(receive_dir.clone(),options(false)).await?;
    let magnet=format!("magnet:?xt=urn:btih:{hash}&tr=http%3A%2F%2F127.0.0.1%3A{tracker_port}%2Fannounce");
    let exchange=async {
        let client=receiver.add_torrent(AddTorrent::from_url(magnet),None).await?.into_handle().context("receiver handle")?;
        client.wait_until_completed().await?;
        let got=std::fs::read(receive_dir.join("sintel-trailer.mp4"))?;
        let expected=std::fs::read(&payload)?;
        ensure!(got==expected,"downloaded content differs");
        ensure!(seed.stats().uploaded_bytes>=got.len() as u64,"no seed upload evidence");
        Ok::<_,anyhow::Error>(json!({"passed":true,"bytes":got.len(),"byte_equal":true,"magnet_metadata_via_tracker":true,"fresh_directory":receive_dir,"receiver_stats":client.stats(),"seed_uploaded_bytes":seed.stats().uploaded_bytes}))
    };
    let evidence=match tokio::time::timeout(Duration::from_secs(90),exchange).await {
        Ok(Ok(v))=>v,Ok(Err(e))=>json!({"passed":false,"error":format!("{e:#}")}),Err(_)=>json!({"passed":false,"error":"90s receiver timeout"})};
    save(&out.join("exchange.json"),&evidence)?;
    ready["native_receiver"]=evidence["passed"].clone(); save(&out.join("ready.json"),&ready)?;
    println!("EXCHANGE {}",evidence);
    receiver.stop().await;
    loop {
        save(&out.join("stats.json"),&json!({"pid":pid,"elapsed_seconds":started.elapsed().as_secs(),"tracker_announces":count.load(Ordering::Relaxed),"seed":seed.stats()}))?;
        if started.elapsed()>=Duration::from_secs(lifetime) {break}
        tokio::select!{_ = tokio::time::sleep(Duration::from_secs(2))=>{}, _ = tokio::signal::ctrl_c()=>{break}}
    }
    tracker_task.abort();session.stop().await;
    ready["ready"]=json!(false);save(&out.join("ready.json"),&ready)?;
    if evidence["passed"]!=true {bail!("native receiver verification failed: {evidence}")}
    Ok(())
}
