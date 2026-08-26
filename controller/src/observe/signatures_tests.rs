use crate::observe::signatures::analyze_rendered_yaml;

#[test]
fn detects_probe_port_mismatch() {
    let yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
spec:
  template:
    spec:
      containers:
        - name: app
          image: ghcr.io/raphael/demo:1.0.0
          ports:
            - containerPort: 8080
          readinessProbe:
            httpGet:
              path: /healthz
              port: 9090
"#;
    let sig = analyze_rendered_yaml(yaml).expect("sig");
    assert_eq!(sig.class, "probe_misconfiguration");
    assert!(sig.key.contains("8080!=9090"));
}

#[test]
fn detects_liveness_too_early() {
    let yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
spec:
  template:
    spec:
      containers:
        - name: app
          image: hashicorp/http-echo:1.0
          env:
            - name: RAPHAEL_LIVENESS_EARLY
              value: "true"
          livenessProbe:
            httpGet:
              path: /healthz
              port: 8080
            initialDelaySeconds: 0
            periodSeconds: 1
            failureThreshold: 1
"#;
    let sig = analyze_rendered_yaml(yaml).expect("sig");
    assert_eq!(sig.class, "probe_misconfiguration");
    assert!(sig.key.contains("liveness_too_early"));
}

#[test]
fn detects_missing_configmap_key() {
    let yaml = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: payments-config
data:
  LOG_LEVEL: info
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
spec:
  template:
    spec:
      containers:
        - name: app
          image: ghcr.io/raphael/demo:1.0.0
          env:
            - name: DATABASE_URL
              valueFrom:
                configMapKeyRef:
                  name: payments-config
                  key: DATABASE_URL
"#;
    let sig = analyze_rendered_yaml(yaml).expect("sig");
    assert_eq!(sig.class, "invalid_missing_config");
}
