<?php
function read_file() {
    $handle = fopen("data.txt", "r");
    if ($handle) {
        $content = fread($handle, filesize("data.txt"));
        fclose($handle);
    }
}

function db_query() {
    $conn = mysqli_connect("localhost", "user", "pass", "db");
    if ($conn) {
        $result = mysqli_query($conn, "SELECT * FROM users");
        mysqli_close($conn);
    }
}
