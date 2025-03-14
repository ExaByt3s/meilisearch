use crate::common::Server;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[actix_rt::test]
async fn error_get_unexisting_task_status() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    let (response, code) = index.get_task(1).await;

    let expected_response = json!({
        "message": "Task `1` not found.",
        "code": "task_not_found",
        "type": "invalid_request",
        "link": "https://docs.meilisearch.com/errors#task_not_found"
    });

    assert_eq!(response, expected_response);
    assert_eq!(code, 404);
}

#[actix_rt::test]
async fn get_task_status() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index
        .add_documents(
            serde_json::json!([{
                "id": 1,
                "content": "foobar",
            }]),
            None,
        )
        .await;
    index.wait_task(0).await;
    let (_response, code) = index.get_task(1).await;
    assert_eq!(code, 200);
    // TODO check resonse format, as per #48
}

#[actix_rt::test]
async fn list_tasks() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    index
        .add_documents(
            serde_json::from_str(include_str!("../assets/test_set.json")).unwrap(),
            None,
        )
        .await;
    let (response, code) = index.list_tasks().await;
    assert_eq!(code, 200);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}

#[actix_rt::test]
async fn list_tasks_with_star_filters() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    index
        .add_documents(
            serde_json::from_str(include_str!("../assets/test_set.json")).unwrap(),
            None,
        )
        .await;
    let (response, code) = index.service.get("/tasks?indexUid=test").await;
    assert_eq!(code, 200);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index.service.get("/tasks?indexUid=*").await;
    assert_eq!(code, 200);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index.service.get("/tasks?indexUid=*,pasteque").await;
    assert_eq!(code, 200);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index.service.get("/tasks?type=*").await;
    assert_eq!(code, 200);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index
        .service
        .get("/tasks?type=*,documentAdditionOrUpdate&status=*")
        .await;
    assert_eq!(code, 200, "{:?}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index
        .service
        .get("/tasks?type=*,documentAdditionOrUpdate&status=*,failed&indexUid=test")
        .await;
    assert_eq!(code, 200, "{:?}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);

    let (response, code) = index
        .service
        .get("/tasks?type=*,documentAdditionOrUpdate&status=*,failed&indexUid=test,*")
        .await;
    assert_eq!(code, 200, "{:?}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}

#[actix_rt::test]
async fn list_tasks_status_filtered() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    index
        .add_documents(
            serde_json::from_str(include_str!("../assets/test_set.json")).unwrap(),
            None,
        )
        .await;

    let (response, code) = index.filtered_tasks(&[], &["succeeded"]).await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 1);

    // We can't be sure that the update isn't already processed so we can't test this
    // let (response, code) = index.filtered_tasks(&[], &["processing"]).await;
    // assert_eq!(code, 200, "{}", response);
    // assert_eq!(response["results"].as_array().unwrap().len(), 1);

    index.wait_task(1).await;

    let (response, code) = index.filtered_tasks(&[], &["succeeded"]).await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}

#[actix_rt::test]
async fn list_tasks_type_filtered() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    index
        .add_documents(
            serde_json::from_str(include_str!("../assets/test_set.json")).unwrap(),
            None,
        )
        .await;

    let (response, code) = index.filtered_tasks(&["indexCreation"], &[]).await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 1);

    let (response, code) = index
        .filtered_tasks(&["indexCreation", "documentAdditionOrUpdate"], &[])
        .await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}

#[actix_rt::test]
async fn list_tasks_status_and_type_filtered() {
    let server = Server::new().await;
    let index = server.index("test");
    index.create(None).await;
    index.wait_task(0).await;
    index
        .add_documents(
            serde_json::from_str(include_str!("../assets/test_set.json")).unwrap(),
            None,
        )
        .await;

    let (response, code) = index.filtered_tasks(&["indexCreation"], &["failed"]).await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 0);

    let (response, code) = index
        .filtered_tasks(
            &["indexCreation", "documentAdditionOrUpdate"],
            &["succeeded", "processing", "enqueued"],
        )
        .await;
    assert_eq!(code, 200, "{}", response);
    assert_eq!(response["results"].as_array().unwrap().len(), 2);
}

macro_rules! assert_valid_summarized_task {
    ($response:expr, $task_type:literal, $index:literal) => {{
        assert_eq!($response.as_object().unwrap().len(), 5);
        assert!($response["taskUid"].as_u64().is_some());
        assert_eq!($response["indexUid"], $index);
        assert_eq!($response["status"], "enqueued");
        assert_eq!($response["type"], $task_type);
        let date = $response["enqueuedAt"].as_str().expect("missing date");

        OffsetDateTime::parse(date, &Rfc3339).unwrap();
    }};
}

#[actix_web::test]
async fn test_summarized_task_view() {
    let server = Server::new().await;
    let index = server.index("test");

    let (response, _) = index.create(None).await;
    assert_valid_summarized_task!(response, "indexCreation", "test");

    let (response, _) = index.update(None).await;
    assert_valid_summarized_task!(response, "indexUpdate", "test");

    let (response, _) = index.update_settings(json!({})).await;
    assert_valid_summarized_task!(response, "settingsUpdate", "test");

    let (response, _) = index.update_documents(json!([{"id": 1}]), None).await;
    assert_valid_summarized_task!(response, "documentAdditionOrUpdate", "test");

    let (response, _) = index.add_documents(json!([{"id": 1}]), None).await;
    assert_valid_summarized_task!(response, "documentAdditionOrUpdate", "test");

    let (response, _) = index.delete_document(1).await;
    assert_valid_summarized_task!(response, "documentDeletion", "test");

    let (response, _) = index.clear_all_documents().await;
    assert_valid_summarized_task!(response, "documentDeletion", "test");

    let (response, _) = index.delete().await;
    assert_valid_summarized_task!(response, "indexDeletion", "test");
}
