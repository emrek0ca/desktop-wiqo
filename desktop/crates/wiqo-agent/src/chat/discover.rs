use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use wiqo_contract::schema_generated::decodes;

pub(super) struct Contract {
    pub method: Method,
    success: &'static str,
    request: Option<&'static str>,
    optional_request: bool,
}

impl Contract {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(if self.success == "DiscoverPostBrief" {
            150
        } else {
            90
        })
    }

    pub fn accepts_request(&self, value: Option<&Value>) -> bool {
        match (self.request, value) {
            (Some(schema), Some(value)) => matches!(decodes(schema, &value.to_string()), Ok(true)),
            (Some(_), None) => self.optional_request,
            (None, value) => value.is_none(),
        }
    }

    pub fn accepts_response(&self, json: &str) -> bool {
        matches!(decodes(self.success, json), Ok(true))
    }
}

pub(super) fn contract(method: &str, path: &str) -> Option<Contract> {
    if path.contains('#') || path.chars().any(char::is_control) {
        return None;
    }
    let route = path.split('?').next()?;
    let parts: Vec<&str> = route.split('/').collect();
    if parts.iter().any(|part| {
        part.is_empty()
            || matches!(*part, "." | "..")
            || !part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    }) {
        return None;
    }
    let method = match method {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        _ => return None,
    };
    let (success, request) = match (method.clone(), parts.as_slice()) {
        (Method::GET, ["profile"]) => ("DiscoverAccountProfile", None),
        (Method::PUT, ["profile"]) => (
            "DiscoverAccountProfile",
            Some("DiscoverAccountProfileWrite"),
        ),
        (Method::GET, ["profile", "cover-library"]) => ("DiscoverCoverLibrary", None),
        (Method::POST, ["missions"]) => ("DiscoverMissionMutation", Some("DiscoverMissionWrite")),
        (Method::GET, ["missions", "feed"]) => ("DiscoverFeed", None),
        (Method::GET, ["missions", "me"]) => ("DiscoverMe", None),
        (Method::PUT, ["missions", "me"]) => {
            ("DiscoverProfileUpdate", Some("DiscoverProfileWrite"))
        }
        (Method::GET, ["missions", "people"]) => ("DiscoverPeople", None),
        (Method::GET, ["missions", "notifications"]) => ("DiscoverNotifications", None),
        (Method::POST, ["missions", "notifications", "read" | "unread"]) => {
            ("DiscoverOk", Some("DiscoverNoticesWrite"))
        }
        (Method::POST, ["missions", "notifications", "delete"]) => {
            ("DiscoverDeleted", Some("DiscoverNoticesWrite"))
        }
        (Method::POST, ["missions", "draft"]) => ("DiscoverDraft", Some("DiscoverDraftWrite")),
        (Method::POST, ["missions", "work-token"]) => {
            ("DiscoverWorkToken", Some("DiscoverWorkTokenWrite"))
        }
        (Method::GET, ["missions", "users", _]) => ("DiscoverProfilePage", None),
        (Method::GET, ["missions", "users", _, "circle"]) => ("DiscoverCircle", None),
        (Method::GET, ["missions", "users", _, "followers" | "following"]) => {
            ("DiscoverPeople", None)
        }
        (Method::POST | Method::DELETE, ["missions", "users", _, "follow" | "block"]) => {
            ("DiscoverOk", None)
        }
        (Method::PATCH, ["missions", "comments", id]) if is_id(id) => {
            ("DiscoverOk", Some("DiscoverCommentWrite"))
        }
        (Method::DELETE, ["missions", "comments" | "images", id]) if is_id(id) => {
            ("DiscoverOk", None)
        }
        (Method::PUT, ["missions", id, "live"]) if is_id(id) => {
            ("DiscoverOk", Some("DiscoverLiveWrite"))
        }
        (Method::POST, ["missions", id, "images"]) if is_id(id) => {
            ("DiscoverImages", Some("DiscoverImageWrite"))
        }
        (Method::PATCH, ["missions", id]) if is_id(id) => {
            ("DiscoverMissionMutation", Some("DiscoverMissionWrite"))
        }
        (Method::DELETE, ["missions", id]) if is_id(id) => ("DiscoverOk", None),
        (Method::GET, ["missions", code]) if is_code(code) => ("DiscoverMissionDetail", None),
        (Method::GET, ["missions", code, "comments"]) if is_code(code) => {
            ("DiscoverComments", None)
        }
        (Method::POST, ["missions", code, "comments"]) if is_code(code) => {
            ("DiscoverCreated", Some("DiscoverCommentWrite"))
        }
        (Method::GET, ["missions", code, "remixes"]) if is_code(code) => {
            ("DiscoverMissionList", None)
        }
        (Method::POST | Method::DELETE, ["missions", code, "like"]) if is_code(code) => {
            ("DiscoverLike", None)
        }
        (Method::POST | Method::DELETE, ["missions", code, "hide"]) if is_code(code) => {
            ("DiscoverOk", None)
        }
        (Method::POST | Method::DELETE, ["missions", code, "save"]) if is_code(code) => {
            ("DiscoverSaved", None)
        }
        (Method::POST, ["missions", code, "verdict"]) if is_code(code) => {
            ("DiscoverVerdictResponse", Some("DiscoverVerdictWrite"))
        }
        (Method::POST, ["missions", code, "copy"]) if is_code(code) => ("DiscoverCopy", None),
        (Method::POST | Method::DELETE, ["missions", code, "hide"]) if is_code(code) => {
            ("DiscoverOk", None)
        }
        (Method::POST, ["missions", code, "report"]) if is_code(code) => {
            ("DiscoverOk", Some("DiscoverReportWrite"))
        }
        (Method::GET, ["missions", "problems", "feed"]) => ("DiscoverPostFeed", None),
        (Method::POST, ["missions", "problems", "similar"]) => {
            ("DiscoverPostSimilar", Some("DiscoverPostMatch"))
        }
        (Method::POST, ["missions", "problems"]) => {
            ("DiscoverPostMutation", Some("DiscoverPostWrite"))
        }
        (Method::DELETE, ["missions", "problems", "replies", id]) if is_id(id) => {
            ("DiscoverOk", None)
        }
        (Method::GET, ["missions", "problems", code]) if is_code(code) => {
            ("DiscoverPostDetail", None)
        }
        (Method::PATCH, ["missions", "problems", code]) if is_code(code) => {
            ("DiscoverPostMutation", Some("DiscoverPostWrite"))
        }
        (Method::DELETE, ["missions", "problems", code]) if is_code(code) => ("DiscoverOk", None),
        (Method::POST, ["missions", "problems", code, "ideas"]) if is_code(code) => {
            ("DiscoverPostMutation", Some("DiscoverPostWrite"))
        }
        (Method::POST | Method::DELETE, ["missions", "problems", code, "need"])
            if is_code(code) =>
        {
            ("DiscoverNeed", None)
        }
        (Method::GET, ["missions", "problems", code, "replies"]) if is_code(code) => {
            ("DiscoverPostReplies", None)
        }
        (Method::POST, ["missions", "problems", code, "replies"]) if is_code(code) => {
            ("DiscoverCreated", Some("DiscoverReplyWrite"))
        }
        (Method::POST, ["missions", "problems", code, "report"]) if is_code(code) => {
            ("DiscoverOk", Some("DiscoverReportWrite"))
        }
        (Method::POST, ["missions", "problems", code, "research"]) if is_code(code) => {
            ("DiscoverPostResearch", None)
        }
        (Method::POST, ["missions", "problems", code, "brief"]) if is_code(code) => {
            ("DiscoverPostBrief", None)
        }
        _ => return None,
    };
    let optional_request = route == "missions/notifications/read";
    Some(Contract {
        method,
        success,
        request,
        optional_request,
    })
}

fn is_id(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_code(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|byte| matches!(byte.to_ascii_uppercase(), b'2'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discover_routes_keep_queries_and_reject_path_traversal() {
        assert!(contract("GET", "missions/feed?q=..%2Filan").is_some());
        assert!(contract("GET", "missions/users/a..b").is_some());
        assert!(contract("GET", "missions/../profile").is_none());
        assert!(contract("GET", "missions/users/%2e%2e").is_none());
        assert!(contract("GET", "missions/users/a%2fb").is_none());
        assert!(contract("GET", "profile/cover-library-other").is_none());
        assert!(contract("GET", "missions/problems/AB23CD45/need").is_none());
        assert!(contract("GET", "missions/problems/AB23CD45#other").is_none());
    }

    #[test]
    fn discover_contract_checks_the_route_specific_request_and_response() {
        let feed = contract("GET", "missions/feed?tab=new").unwrap();
        assert!(feed.accepts_response(r#"{"missions":[],"categories":[],"next":null}"#));
        assert!(!feed.accepts_response(r#"{"posts":[],"topics":[],"next":null}"#));

        let reply = contract("POST", "missions/problems/AB23CD45/replies").unwrap();
        assert!(reply.accepts_request(Some(&json!({"body": "Example", "kind": "experience"}))));
        assert!(!reply.accepts_request(Some(&json!({"body": 1}))));
        assert!(!reply.accepts_request(None));
        assert!(reply.accepts_response(r#"{"id":4}"#));

        let read = contract("POST", "missions/notifications/read").unwrap();
        assert!(read.accepts_request(None));
        assert!(read.accepts_request(Some(&json!({"ids": [1, 2]}))));
    }
}
