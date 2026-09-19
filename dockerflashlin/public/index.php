<?php
/**
 * FlashLin Docker — PHP-FPM connectivity test.
 * Remove this file in production.
 */

$info = [
    'php_version'    => PHP_VERSION,
    'server_name'    => $_SERVER['SERVER_SOFTWARE'] ?? 'unknown',
    'request_method' => $_SERVER['REQUEST_METHOD'] ?? '',
    'request_uri'    => $_SERVER['REQUEST_URI'] ?? '',
    'document_root'  => $_SERVER['DOCUMENT_ROOT'] ?? '',
    'script_filename'=> $_SERVER['SCRIPT_FILENAME'] ?? '',
    'remote_addr'    => $_SERVER['REMOTE_ADDR'] ?? '',
    'http_host'      => $_SERVER['HTTP_HOST'] ?? '',
    'gateway'        => $_SERVER['GATEWAY_INTERFACE'] ?? '',
    'timestamp'      => date('Y-m-d H:i:s T'),
];
?>
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>FlashLin — PHP Works!</title>
    <style>
        body { font-family: monospace; background: #0f1117; color: #e2e8f0; padding: 2rem; }
        h1   { color: #6366f1; margin-bottom: 1.5rem; }
        table { border-collapse: collapse; width: 100%; max-width: 700px; }
        th, td { text-align: left; padding: 0.5rem 1rem; border: 1px solid #2d3748; }
        th { background: #1a1f2e; color: #94a3b8; width: 200px; }
        td { color: #38bdf8; }
    </style>
</head>
<body>
    <h1>FlashLin Docker — PHP Works!</h1>
    <p style="color:#94a3b8;margin-bottom:1.5rem">
        PHP is connected via PHP-FPM / FastCGI and processing requests correctly.
    </p>
    <table>
        <?php foreach ($info as $key => $value): ?>
        <tr>
            <th><?= htmlspecialchars($key) ?></th>
            <td><?= htmlspecialchars((string) $value) ?></td>
        </tr>
        <?php endforeach; ?>
    </table>
    <p style="color:#475569;margin-top:2rem;font-size:0.8rem">
        Remove index.php in production to avoid information disclosure.
    </p>
</body>
</html>
