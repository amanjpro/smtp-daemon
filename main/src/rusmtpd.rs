extern crate serde_json;
extern crate common;
extern crate secstr;
extern crate esmtp_client;

use secstr::SecStr;
use common::*;
use esmtp_client::SMTPConnection;

use std::os::unix::net::{UnixStream, UnixListener};
use std::net::Shutdown;
use std::process::{Command, Stdio};
use std::io::{Read, Write};
use std::error::Error;
use std::{str,fs};
use std::thread;
use std::time::Duration;
use std::sync::{Mutex, Arc};
use std::ops::Deref;


pub struct ExternalClient {
    pub client: String,
}

impl ExternalClient {
    fn send_mail(&self, mut stream: UnixStream, passwd: &SecStr) {
        let mut mail = String::new();
        let _ = stream.read_to_string(&mut mail).unwrap();
        let mail: Mail = serde_json::from_str(&mail).expect("Cannot parse the mail");
        let recipients: Vec<String> = mail.recipients;
        let body = mail.body;

        let smtp = Command::new(&self.client)
          .arg(format!("--passwordeval=echo {}", str::from_utf8(passwd.unsecure()).unwrap()))
          .args(recipients)
          .stdin(Stdio::piped())
          .stdout(Stdio::null())
          .spawn()
          .expect("Failed to start smtp process");

        match smtp.stdin.unwrap().write_all(body.as_slice()) {
            Err(why) => {
                let _ = stream.write_all(ERROR_SIGNAL.as_bytes());
                panic!("couldn't write to smtp stdin: {}", why.description());
            },
            Ok(_) => {
                let _ = stream.write_all(OK_SIGNAL.as_bytes());
                println!("email sent to smtp");
            },
        }
    }

    pub fn start(&self, label: &str, vault: &Vault, passwd: Vec<u8>) {
        if let Ok(listener) = UnixListener::bind(get_socket_path(&label)) {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                      let decrypted = vault.decrypt(passwd.clone());
                      self.send_mail(stream, &SecStr::from(decrypted));
                    }
                    _              => {
                        /* connection failed */
                        break;
                    }
                }
            }
        } else {
            panic!("failed to open a socket")
        }
    }

    pub fn new(client: &str) -> Self {
        ExternalClient { client: client.to_string() }
    }
}

pub struct DefaultClient {
    account: Account,
}

impl DefaultClient {
    fn get_mailer(&self, vault: &Vault, passwd: Vec<u8>) -> Arc<Mutex<SMTPConnection>> {
        let account = &self.account;

        let label    = &account.label.to_string();

        let host     = &account.host
            .as_ref()
            .expect(&format!("Please configure the host for {}", label));

        let username = account.username
            .as_ref()
            .expect(&format!("Please configure the username for {}", label));

        let port     = account.port
            .as_ref()
            .expect(&format!("Please configure the port for {}", label));

        let mut mailer = SMTPConnection::open_connection(&host, *port);

        if mailer.supports_login {
            mailer.login(&SecStr::from(username.clone()), &SecStr::from(vault.decrypt(passwd.clone())));
        }

        Arc::new(Mutex::new(mailer))
    }

    fn maintain_connection(&self, mailer: Arc<Mutex<SMTPConnection>>, heartbeat: u64) {
        thread::spawn(move || {
            let sleep_time = Duration::from_secs(heartbeat * 60);
            loop {
                mailer.lock().expect("Cannot get the mailer instance to keep it alive")
                    .keep_alive(); thread::sleep(sleep_time)
            }
        });
    }

    fn start(&self, vault: &Vault) {
        let account = &self.account;

        let label = &account.label;

        let password = account.password.clone()
            .expect(&format!("Password is not defined for {}", &label));

        let mailer = self.get_mailer(vault, password.clone());

        match &account.mode {
            AccountMode::Paranoid =>
                {
                    let mailer = mailer.clone();
                    let heartbeat = &account.heartbeat;
                    &self.maintain_connection(mailer, *heartbeat);
                },
            _                     => (),
        }

        if let Ok(listener) = UnixListener::bind(get_socket_path(&label)) {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let mailer = if account.mode == AccountMode::Secure {
                            self.get_mailer(vault, password.clone())
                        } else { mailer.clone() };

                        let username = &account.username
                            .as_ref()
                            .expect(&format!("Please configure the username for {}", &label));
                        let mut mail = String::new();
                        stream.read_to_string(&mut mail).unwrap();
                        let mail: Mail = serde_json::from_str(&mail).expect("Cannot parse the mail");
                        let recipients: Vec<&str> = mail.recipients.iter().filter(|&s| s != "--").map(|s| s.deref()).collect();
                        let body = mail.body;
                        mailer.lock().expect("Cannot get the mailer instance to send an email")
                            .send_mail(&username, &recipients, &body);
                        let _ = stream.write_all(OK_SIGNAL.as_bytes());
                        if &account.mode == &AccountMode::Secure {
                            stream.shutdown(Shutdown::Both).expect("shutdown function failed");
                        }

                    }
                    _          => {
                        /* connection failed */
                        break;
                    }
                }
            }
        } else {
            panic!("failed to open a socket")
        }
    }

    pub fn new(account: Account) -> Self {
        DefaultClient { account: account }
    }
}

fn start_daemon(conf: Configuration) {
    let mut children = vec![];
    for account in conf.accounts {
        let client = conf.smtpclient.clone();
        children.push(thread::spawn(move || {
            let eval = account.passwordeval.clone();

            if let Ok(result) = Command::new("sh").arg("-c").arg(eval).stdout(Stdio::piped()).spawn() {
                let mut child_stdout = result.stdout.expect("Cannot get the handle of the child process");
                let mut passwd = String::new();
                let _ = child_stdout.read_to_string(&mut passwd);


                // close the socket, if it exists
                let _ = fs::remove_file(get_socket_path(&account.label));

                let account = if account.mode == AccountMode::Secure {
                    Account {
                        label: account.label,
                        username: account.username,
                        passwordeval: account.passwordeval,
                        mode: account.mode,
                        host: account.host,
                        port: account.port,
                        tls: account.tls,
                        heartbeat: account.heartbeat,
                        default: account.default,
                        password: Some(account.vault.encrypt(&mut passwd)),
                        vault: account.vault,
                    }
                } else {
                    account
                };

                match client {
                    Some(client) => {
                        let external_client = ExternalClient::new(&client);
                        external_client.start(&account.label, &account.vault, passwd.as_bytes().to_vec());
                    },
                    None         => {
                        let default_client = DefaultClient::new(account);
                        default_client.start(&default_client.account.vault);
                    },
                }
            }
        }));
    }

    for child in children {
        // Wait for the thread to finish. Returns a result.
        let _ = child.join();
    }
}

fn main() {

    let args = process_args("rusmtpd", &rusmtpd_usage("rusmtpd"));
    let conf = read_config(&args.flag_rusmtprc);

    start_daemon(conf);
}

