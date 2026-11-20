//! Mail notifications from the outbox. The projector queues a message when an event calls for
//! one (a passed gate for reviewers; a review, a merge approval or a comment for the author);
//! this task sends them. The outbox survives a rebuild of the views, so nobody is mailed twice.

use crate::config::MailConfig;
use crate::db;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use std::sync::Arc;
use std::time::Duration;
use tokio_postgres::Client;

fn transport(m: &MailConfig) -> anyhow::Result<SmtpTransport> {
    let b = match m.tls.as_str() {
        "none" => SmtpTransport::builder_dangerous(&m.host),
        "tls" => SmtpTransport::relay(&m.host)?,
        _ => SmtpTransport::starttls_relay(&m.host)?,
    };
    let mut b = b.port(m.port).timeout(Some(Duration::from_secs(20)));
    if let Some(u) = &m.username {
        b = b.credentials(Credentials::new(u.clone(), m.password.clone()));
    }
    Ok(b.build())
}

fn message(m: &MailConfig, to: &str, subject: &str, body: &str) -> anyhow::Result<Message> {
    Ok(Message::builder()
        .from(m.from.parse::<Mailbox>()?)
        .to(to.parse::<Mailbox>()?)
        .subject(format!("[egm-team] {subject}"))
        .body(body.to_string())?)
}

/// Send what is queued, every two seconds.
pub async fn sender(m: MailConfig, public_url: String, db: Arc<Client>) {
    let smtp = match transport(&m) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            eprintln!("egm-team mail: {e}; notifications are off");
            return;
        }
    };
    loop {
        match db::outbox_pending(&*db).await {
            Ok(rows) => {
                for (id, to, subject, body) in rows {
                    let body = body.replace("{url}", &public_url);
                    let (m2, smtp2, to2) = (m.clone(), smtp.clone(), to.clone());
                    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        smtp2.send(&message(&m2, &to2, &subject, &body)?)?;
                        Ok(())
                    })
                    .await;
                    let err = match res {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(e.to_string()),
                        Err(e) => Some(e.to_string()),
                    };
                    if let Some(e) = &err {
                        eprintln!("egm-team mail to {to}: {e}");
                    }
                    let _ = db::outbox_done(&*db, id, &to, err).await;
                }
            }
            Err(e) => eprintln!("egm-team mail: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
