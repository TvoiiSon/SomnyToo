use std::sync::Arc;
use std::time::{Instant, Duration};
use tokio::sync::RwLock;
use tracing::{info, debug};
use dashmap::DashMap;

/// Параметры модели времени обработки батча
/// T_process(B) = α/B + β + γ·B + δ·(B - B_opt)² + ε·I(B > L3_CACHE)
#[derive(Debug, Clone, Copy)]
pub struct BatchModelParams {
    pub alpha: f64,        // Накладные расходы на батч (мс)
    pub beta: f64,         // Минимальное время обработки (мс)
    pub gamma: f64,        // Линейный коэффициент (мс/B)
    pub delta: f64,        // Квадратичный штраф (мс/B²)
    pub b_opt: f64,       // Оптимальный размер для кэша
    pub epsilon: f64,      // Штраф за выход из L3-кэша
    pub l3_cache_size: usize, // Размер L3-кэша в байтах
    pub confidence: f64,   // Уверенность в оценке (0..1)
}

impl Default for BatchModelParams {
    fn default() -> Self {
        Self {
            alpha: 0.5,      // 0.5 мс накладных
            beta: 0.05,      // 0.05 мс минимум
            gamma: 0.001,    // 0.001 мс на элемент
            delta: 0.0001,   // БЫЛО: 0.00001, СТАЛО: 0.0001 (сильнее штрафуем большие батчи)
            b_opt: 256.0,    // оптимально 256
            epsilon: 0.1,
            l3_cache_size: 8 * 1024 * 1024,
            confidence: 0.5,
        }
    }
}

impl BatchModelParams {
    /// Время обработки батча размера B
    pub fn processing_time(&self, b: usize) -> f64 {
        let b_f64 = b as f64;
        let cache_penalty = if b > self.l3_cache_size / 64 { // ~64 байта на элемент
            self.epsilon
        } else {
            0.0
        };

        self.alpha / b_f64 +
            self.beta +
            self.gamma * b_f64 +
            self.delta * (b_f64 - self.b_opt).powi(2) +
            cache_penalty
    }
}

/// Решение кубического уравнения a·x³ + b·x² + c·x + d = 0
/// Возвращает действительные корни
pub fn solve_cubic(a: f64, b: f64, c: f64, d: f64) -> Vec<f64> {
    if a.abs() < 1e-12 {
        // Квадратное уравнение
        if b.abs() < 1e-12 {
            return vec![];
        }
        let disc = c * c - 4.0 * b * d;
        if disc < 0.0 {
            return vec![];
        }
        return vec![
            (-c + disc.sqrt()) / (2.0 * b),
            (-c - disc.sqrt()) / (2.0 * b)
        ];
    }

    // Приведение к нормализованному виду: x³ + px + q = 0
    let p = (3.0 * a * c - b * b) / (3.0 * a * a);
    let q = (2.0 * b * b * b - 9.0 * a * b * c + 27.0 * a * a * d) / (27.0 * a * a * a);

    let discriminant = (q * q / 4.0) + (p * p * p / 27.0);

    if discriminant > 0.0 {
        // Один действительный корень
        let sqrt_d = discriminant.sqrt();
        let u = (-q / 2.0 + sqrt_d).cbrt();
        let v = (-q / 2.0 - sqrt_d).cbrt();
        let x1 = u + v - b / (3.0 * a);
        vec![x1]
    } else if discriminant.abs() < 1e-12 {
        // Кратные корни
        let u = (-q / 2.0).cbrt();
        let x1 = 2.0 * u - b / (3.0 * a);
        let x2 = -u - b / (3.0 * a);
        vec![x1, x2]
    } else {
        // Три действительных корня
        let r = (-p * p * p / 27.0).sqrt();
        let phi = (-q / (2.0 * r)).acos();
        let sqrt_r = r.cbrt();

        let x1 = 2.0 * sqrt_r * (phi / 3.0).cos() - b / (3.0 * a);
        let x2 = 2.0 * sqrt_r * ((phi + 2.0 * std::f64::consts::PI) / 3.0).cos() - b / (3.0 * a);
        let x3 = 2.0 * sqrt_r * ((phi + 4.0 * std::f64::consts::PI) / 3.0).cos() - b / (3.0 * a);
        vec![x1, x2, x3]
    }
}

#[derive(Debug, Clone)]
pub struct KalmanFilter {
    pub state: f64,        // Текущая оценка λ
    pub covariance: f64,   // Дисперсия оценки
    pub process_noise: f64, // Шум процесса (Q)
    pub measurement_noise: f64, // Шум измерения (R)
}

impl KalmanFilter {
    pub fn new(initial_lambda: f64) -> Self {
        Self {
            state: initial_lambda,
            covariance: 1.0,
            process_noise: 0.01,
            measurement_noise: 1.0,
        }
    }

    /// Предсказание следующего состояния
    pub fn predict(&mut self) {
        self.covariance += self.process_noise;
    }

    /// Обновление на основе измерения
    pub fn update(&mut self, measurement: f64) -> f64 {
        let kalman_gain = self.covariance / (self.covariance + self.measurement_noise);
        self.state += kalman_gain * (measurement - self.state);
        self.covariance = (1.0 - kalman_gain) * self.covariance;
        self.state
    }
}

#[derive(Debug, Clone)]
pub struct MarkovChain2nd {
    // Уровни квантования нагрузки
    pub levels: usize,
    // Матрица переходов P[λ_{t-1}, λ_t, λ_{t+1}]
    pub transitions: Vec<Vec<Vec<f64>>>,
    // Счетчики для обучения
    pub counts: Vec<Vec<Vec<u64>>>,
}

impl MarkovChain2nd {
    pub fn new(levels: usize) -> Self {
        let transitions = vec![vec![vec![0.0; levels]; levels]; levels];
        let counts = vec![vec![vec![0; levels]; levels]; levels];

        Self {
            levels,
            transitions,
            counts,
        }
    }

    /// Обновление цепи новым наблюдением
    pub fn update(&mut self, l_tm1: usize, l_t: usize, l_tp1: usize) {
        if l_tm1 < self.levels && l_t < self.levels && l_tp1 < self.levels {
            self.counts[l_tm1][l_t][l_tp1] += 1;

            // Пересчет вероятностей
            let total: u64 = self.counts[l_tm1][l_t].iter().sum();
            if total > 0 {
                for k in 0..self.levels {
                    self.transitions[l_tm1][l_t][k] =
                        self.counts[l_tm1][l_t][k] as f64 / total as f64;
                }
            }
        }
    }

    /// Предсказание следующего состояния
    pub fn predict(&self, l_tm1: usize, l_t: usize) -> (usize, f64) {
        if l_tm1 >= self.levels || l_t >= self.levels {
            return (self.levels / 2, 0.5);
        }

        let mut max_prob = 0.0;
        let mut best = self.levels / 2;

        for k in 0..self.levels {
            let prob = self.transitions[l_tm1][l_t][k];
            if prob > max_prob {
                max_prob = prob;
                best = k;
            }
        }

        (best, max_prob)
    }

    /// Квантование нагрузки в уровень
    pub fn quantize(&self, lambda: f64, max_lambda: f64) -> usize {
        if lambda <= 0.0 { return 0; }
        let idx = (lambda / max_lambda * (self.levels - 1) as f64) as usize;
        idx.min(self.levels - 1)
    }

    /// Деквантование уровня в нагрузку
    pub fn dequantize(&self, level: usize, max_lambda: f64) -> f64 {
        (level as f64 + 0.5) * max_lambda / self.levels as f64
    }
}

#[derive(Debug, Clone)]
pub struct WaveletTransformer {
    // Коэффициенты фильтра Добеши 4
    h0: f64, h1: f64, h2: f64, h3: f64,
    g0: f64, g1: f64, g2: f64, g3: f64,

    // Максимальный уровень разложения
    pub max_level: usize,

    // История наблюдений
    history: Vec<f64>,
    max_history: usize,

    // Вейвлет-коэффициенты
    approx: Vec<Vec<f64>>,  // a_j,k
    detail: Vec<Vec<f64>>,  // d_j,k
}

impl WaveletTransformer {
    pub fn new(max_level: usize, max_history: usize) -> Self {
        // Коэффициенты Добеши 4
        let sqrt3 = 3.0f64.sqrt();
        let h0 = (1.0 + sqrt3) / (4.0 * 2.0f64.sqrt());
        let h1 = (3.0 + sqrt3) / (4.0 * 2.0f64.sqrt());
        let h2 = (3.0 - sqrt3) / (4.0 * 2.0f64.sqrt());
        let h3 = (1.0 - sqrt3) / (4.0 * 2.0f64.sqrt());

        let g0 = h3;
        let g1 = -h2;
        let g2 = h1;
        let g3 = -h0;

        Self {
            h0, h1, h2, h3,
            g0, g1, g2, g3,
            max_level,
            history: Vec::with_capacity(max_history),
            max_history,
            approx: vec![Vec::new(); max_level + 1],
            detail: vec![Vec::new(); max_level + 1],
        }
    }

    /// Добавление нового наблюдения
    pub fn add_observation(&mut self, value: f64) {
        self.history.push(value);
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        if self.history.len() >= 4 {
            self.transform();
        }
    }

    /// Дискретное вейвлет-преобразование
    pub fn transform(&mut self) {
        if self.history.len() < 4 {
            return;
        }

        let mut current = self.history.clone();

        for level in 0..self.max_level {
            if current.len() < 4 {
                break;
            }

            let mut approx = Vec::new();
            let mut detail = Vec::new();

            for i in (0..current.len() - 3).step_by(2) {
                let a = self.h0 * current[i] +
                    self.h1 * current[i + 1] +
                    self.h2 * current[i + 2] +
                    self.h3 * current[i + 3];

                let d = self.g0 * current[i] +
                    self.g1 * current[i + 1] +
                    self.g2 * current[i + 2] +
                    self.g3 * current[i + 3];

                approx.push(a);
                detail.push(d);
            }

            self.approx[level] = approx.clone();
            self.detail[level] = detail;
            current = approx;
        }
    }

    /// Предсказание на τ шагов вперед
    pub fn predict(&self, tau: usize) -> Vec<f64> {
        if self.approx[0].is_empty() {
            return vec![0.0; tau];
        }

        let mut predictions = Vec::with_capacity(tau);
        let last_approx = self.approx[0].last().copied().unwrap_or(0.0);
        let last_detail = self.detail[0].last().copied().unwrap_or(0.0);

        // Линейная экстраполяция по последним коэффициентам
        for i in 0..tau {
            let pred = last_approx + last_detail * (i + 1) as f64;
            predictions.push(pred.max(0.0));
        }

        predictions
    }
}

#[derive(Debug, Clone)]
pub struct PIDController {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,

    integral: f64,
    prev_error: f64,
    prev_time: Instant,
    initialized: bool,

    // Ограничения
    pub output_min: f64,
    pub output_max: f64,
    pub integral_limit: f64,
}

impl PIDController {
    pub fn new(kp: f64, ki: f64, kd: f64) -> Self {
        Self {
            kp, ki, kd,
            integral: 0.0,
            prev_error: 0.0,
            prev_time: Instant::now(),
            initialized: false,
            output_min: -100.0,
            output_max: 100.0,
            integral_limit: 1000.0,
        }
    }

    /// Автонастройка методом Циглера-Николса
    pub fn auto_tune(kp_crit: f64, t_crit: f64) -> Self {
        Self {
            kp: 0.6 * kp_crit,
            ki: 2.0 * (0.6 * kp_crit) / t_crit,
            kd: (0.6 * kp_crit) * t_crit / 8.0,
            integral: 0.0,
            prev_error: 0.0,
            prev_time: Instant::now(),
            initialized: false,
            output_min: -100.0,
            output_max: 100.0,
            integral_limit: 1000.0,
        }
    }

    /// Вычисление управляющего сигнала
    pub fn compute(&mut self, error: f64) -> f64 {
        let now = Instant::now();
        let dt = if self.initialized {
            now.duration_since(self.prev_time).as_secs_f64().max(0.001)
        } else {
            self.initialized = true;
            0.001
        };

        // Интегральная составляющая с ограничением
        self.integral += error * dt;
        self.integral = self.integral.clamp(-self.integral_limit, self.integral_limit);

        // Дифференциальная составляющая
        let derivative = if self.initialized {
            (error - self.prev_error) / dt
        } else {
            0.0
        };

        // ПИД-выход
        let output = self.kp * error + self.ki * self.integral + self.kd * derivative;

        self.prev_error = error;
        self.prev_time = now;

        output.clamp(self.output_min, self.output_max)
    }

    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
        self.initialized = false;
    }
}

#[derive(Debug, Clone)]
pub struct GibbsSampler {
    pub alpha_samples: Vec<f64>,
    pub beta_samples: Vec<f64>,
    pub gamma_samples: Vec<f64>,
    pub delta_samples: Vec<f64>,
    pub b_opt_samples: Vec<f64>,

    // Параметры априорных распределений (Gamma)
    alpha_a: f64, alpha_b: f64,
    beta_a: f64, beta_b: f64,
    gamma_a: f64, gamma_b: f64,
    delta_a: f64, delta_b: f64,

    // Текущие значения
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
    pub delta: f64,
    pub b_opt: f64,
}

impl GibbsSampler {
    pub fn new() -> Self {
        Self {
            alpha_samples: Vec::with_capacity(1000),
            beta_samples: Vec::with_capacity(1000),
            gamma_samples: Vec::with_capacity(1000),
            delta_samples: Vec::with_capacity(1000),
            b_opt_samples: Vec::with_capacity(1000),

            // Априорные: Gamma(1, 0.001) и т.д.
            alpha_a: 1.0, alpha_b: 0.001,
            beta_a: 1.0, beta_b: 0.0001,
            gamma_a: 1.0, gamma_b: 0.00001,
            delta_a: 1.0, delta_b: 0.000001,

            alpha: 0.5,
            beta: 0.05,
            gamma: 0.001,
            delta: 0.00001,
            b_opt: 256.0,
        }
    }

    /// Одна итерация сэмплирования Гиббса
    pub fn sample(&mut self, data: &[(usize, f64)]) {
        if data.is_empty() {
            return;
        }

        let n = data.len() as f64;
        let sum_1_b: f64 = data.iter().map(|(b, _)| 1.0 / *b as f64).sum();
        let sum_b: f64 = data.iter().map(|(b, _)| *b as f64).sum();
        let _sum_b2: f64 = data.iter().map(|(b, _)| (*b as f64).powi(2)).sum();
        let sum_t: f64 = data.iter().map(|(_, t)| *t).sum();
        let sum_t_div_b: f64 = data.iter().map(|(b, t)| t / *b as f64).sum();
        let sum_t_b: f64 = data.iter().map(|(b, t)| t * *b as f64).sum();

        // Сэмплирование alpha | rest
        let alpha_shape = self.alpha_a + n;
        let alpha_rate = self.alpha_b + sum_t_div_b;
        self.alpha = self.sample_gamma(alpha_shape, 1.0 / alpha_rate);

        // Сэмплирование beta | rest
        let beta_shape = self.beta_a + n;
        let beta_rate = self.beta_b + sum_t - self.alpha * sum_1_b - self.gamma * sum_b
            - self.delta * data.iter().map(|(b, _)| (*b as f64 - self.b_opt).powi(2)).sum::<f64>();
        self.beta = self.sample_gamma(beta_shape, 1.0 / beta_rate.max(0.001));

        // Сэмплирование gamma | rest
        let gamma_shape = self.gamma_a + sum_b;
        let gamma_rate = self.gamma_b + (sum_t_b - self.alpha * n - self.beta * sum_b
            - self.delta * data.iter().map(|(b, _)| (*b as f64 - self.b_opt).powi(2) * *b as f64).sum::<f64>());
        self.gamma = self.sample_gamma(gamma_shape, 1.0 / gamma_rate.max(0.001));

        // Сэмплирование delta | rest
        let sum_b_bopt2: f64 = data.iter().map(|(b, _)| (*b as f64 - self.b_opt).powi(2)).sum();
        let delta_shape = self.delta_a + sum_b_bopt2;
        let delta_rate = self.delta_b + data.iter().map(|(b, t)| {
            (t - self.alpha / *b as f64 - self.beta - self.gamma * *b as f64) * (*b as f64 - self.b_opt).powi(2)
        }).sum::<f64>();
        self.delta = self.sample_gamma(delta_shape, 1.0 / delta_rate.max(0.001));

        // Сэмплирование b_opt (метрополис-гастингс)
        let b_opt_new = self.b_opt + rand::random::<f64>() * 10.0 - 5.0;
        if b_opt_new > 32.0 && b_opt_new < 1024.0 {
            let ll_current = self.log_likelihood(data);
            self.b_opt = b_opt_new;
            let ll_new = self.log_likelihood(data);

            if ll_new < ll_current && rand::random::<f64>() > (ll_new - ll_current).exp() {
                self.b_opt = b_opt_new - 10.0; // reject
            }
        }
    }

    /// Логарифм правдоподобия
    fn log_likelihood(&self, data: &[(usize, f64)]) -> f64 {
        data.iter().map(|(b, t)| {
            let pred = self.alpha / *b as f64 + self.beta + self.gamma * *b as f64
                + self.delta * (*b as f64 - self.b_opt).powi(2);
            -0.5 * (t - pred).powi(2)
        }).sum()
    }

    /// Сэмплирование из Gamma распределения (метод Марсальи)
    fn sample_gamma(&self, shape: f64, scale: f64) -> f64 {
        if shape < 1.0 {
            let u = rand::random::<f64>();
            return self.sample_gamma(1.0 + shape, scale) * u.powf(1.0 / shape);
        }

        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();

        loop {
            let x = rand::random::<f64>();
            let v = (1.0 - c * (1.0 - x).ln() / x).powi(3);

            if v > 0.0 && rand::random::<f64>() < 1.0 - 0.0331 * (x * x * x).powi(2) {
                return d * v * scale;
            }
        }
    }

    /// Запуск цепочки сэмплирования
    pub fn run_chain(&mut self, data: &[(usize, f64)], iterations: usize, burn_in: usize) {
        self.alpha_samples.clear();
        self.beta_samples.clear();
        self.gamma_samples.clear();
        self.delta_samples.clear();
        self.b_opt_samples.clear();

        for i in 0..iterations {
            self.sample(data);

            if i >= burn_in {
                self.alpha_samples.push(self.alpha);
                self.beta_samples.push(self.beta);
                self.gamma_samples.push(self.gamma);
                self.delta_samples.push(self.delta);
                self.b_opt_samples.push(self.b_opt);
            }
        }
    }

    /// Усреднение сэмплов
    pub fn average(&self) -> (f64, f64, f64, f64, f64) {
        let alpha_mean = self.alpha_samples.iter().sum::<f64>() / self.alpha_samples.len() as f64;
        let beta_mean = self.beta_samples.iter().sum::<f64>() / self.beta_samples.len() as f64;
        let gamma_mean = self.gamma_samples.iter().sum::<f64>() / self.gamma_samples.len() as f64;
        let delta_mean = self.delta_samples.iter().sum::<f64>() / self.delta_samples.len() as f64;
        let b_opt_mean = self.b_opt_samples.iter().sum::<f64>() / self.b_opt_samples.len() as f64;

        (alpha_mean, beta_mean, gamma_mean, delta_mean, b_opt_mean)
    }
}

#[derive(Debug, Clone)]
pub struct GPScheduler {
    // Веса приоритетов
    pub weights: [f64; 5],  // Critical, High, Normal, Low, Background

    // Текущие доли пропускной способности
    pub shares: [f64; 5],

    // Очереди по приоритетам
    pub queue_lengths: [f64; 5],

    // Общая пропускная способность
    pub total_capacity: f64,
}

impl GPScheduler {
    pub fn new(total_capacity: f64) -> Self {
        Self {
            weights: [4.0, 2.0, 1.0, 0.5, 0.25],
            shares: [0.0; 5],
            queue_lengths: [0.0; 5],
            total_capacity,
        }
    }

    /// Пересчёт долей пропускной способности
    pub fn recompute_shares(&mut self) {
        let sum_weights: f64 = self.weights.iter().sum();
        for i in 0..5 {
            self.shares[i] = self.weights[i] * self.total_capacity / sum_weights;
        }
    }

    /// Время ожидания для класса i (формула BCMP)
    pub fn waiting_time(&self, i: usize, lambda: f64, batch_size: f64, service_rate: f64) -> f64 {
        let rho_i = lambda * batch_size / self.shares[i];
        let total_rho: f64 = (0..5).map(|j| {
            if j == i {
                rho_i
            } else {
                self.queue_lengths[j] * batch_size / self.shares[j]
            }
        }).sum();

        (1.0 + total_rho) / (service_rate * (1.0 - total_rho))
    }
}

#[derive(Debug, Clone)]
pub struct ModelPredictiveController {
    pub horizon: usize,
    pub lambda_pred: Vec<f64>,
    pub params: BatchModelParams,

    // Веса в целевой функции
    pub w_latency: f64,
    pub w_delta_b: f64,
    pub w_delta_m: f64,

    // Ограничения
    pub b_min: usize,
    pub b_max: usize,
    pub m_min: usize,
    pub m_max: usize,
}

impl ModelPredictiveController {
    pub fn new(horizon: usize, params: BatchModelParams) -> Self {
        Self {
            horizon,
            lambda_pred: vec![0.0; horizon],
            params,
            w_latency: 1.0,
            w_delta_b: 0.1,
            w_delta_m: 0.5,
            b_min: 32,
            b_max: 1024,
            m_min: 1,
            m_max: 64,
        }
    }

    /// Решение задачи квадратичного программирования для B и M
    pub fn solve(&self, current_b: usize, current_m: usize) -> (usize, usize) {
        let mut best_b = current_b;
        let mut best_m = current_m;
        let mut best_cost = f64::INFINITY;

        // Поиск по сетке (для реального времени)
        let b_candidates = [
            current_b.saturating_sub(64).max(self.b_min),
            current_b.saturating_sub(32).max(self.b_min),
            current_b,
            (current_b + 32).min(self.b_max),
            (current_b + 64).min(self.b_max),
        ];

        let m_candidates = [
            current_m.saturating_sub(2).max(self.m_min),
            current_m,
            (current_m + 2).min(self.m_max),
        ];

        for &b in &b_candidates {
            for &m in &m_candidates {
                let cost = self.cost_function(b as f64, m as f64, current_b as f64, current_m as f64);
                if cost < best_cost {
                    best_cost = cost;
                    best_b = b;
                    best_m = m;
                }
            }
        }

        (best_b, best_m)
    }

    /// Целевая функция MPC
    fn cost_function(&self, b: f64, m: f64, b_prev: f64, m_prev: f64) -> f64 {
        let mut total_cost = 0.0;

        for k in 0..self.horizon {
            let lambda_k = self.lambda_pred.get(k).copied().unwrap_or(1.0).max(0.1);

            // Latency на шаге k
            let t_batch = b / lambda_k;
            let t_process = self.params.processing_time(b as usize);
            let t_queue = 1.0 / (m * 1000.0); // Упрощённо
            let latency = t_batch + t_process + t_queue;

            // Штрафы за изменения
            let delta_b = if k == 0 { (b - b_prev).abs() } else { 0.0 };
            let delta_m = if k == 0 { (m - m_prev).abs() } else { 0.0 };

            total_cost += self.w_latency * latency +
                self.w_delta_b * delta_b +
                self.w_delta_m * delta_m;
        }

        total_cost
    }

    pub fn set_lambda_predictions(&mut self, predictions: Vec<f64>) {
        self.lambda_pred = predictions;
    }
}

#[derive(Debug, Clone)]
pub struct ExtremeValueTheory {
    pub threshold: f64,     // Порог u
    pub exceedances: Vec<f64>, // Превышения над порогом
    pub xi: f64,           // Параметр формы
    pub sigma: f64,        // Параметр масштаба
}

impl ExtremeValueTheory {
    pub fn new() -> Self {
        Self {
            threshold: 0.0,
            exceedances: Vec::new(),
            xi: 0.0,
            sigma: 1.0,
        }
    }

    /// Оценка параметров GPD методом Пикандса
    pub fn fit(&mut self, data: &[f64], percentile: f64) {
        if data.len() < 100 {
            return;
        }

        let mut sorted = data.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Порог = 95-й перцентиль
        let p_idx = (sorted.len() as f64 * percentile) as usize;
        self.threshold = sorted[p_idx];

        // Превышения
        self.exceedances = sorted[p_idx..]
            .iter()
            .map(|&x| x - self.threshold)
            .filter(|&x| x > 0.0)
            .collect();

        if self.exceedances.len() < 10 {
            return;
        }

        // Метод Пикандса для ξ
        let n = self.exceedances.len();
        let k1 = (n as f64 * 0.5) as usize;
        let k2 = (n as f64 * 0.25) as usize;
        let k3 = (n as f64 * 0.125) as usize;

        let q1 = self.exceedances[k1.min(self.exceedances.len() - 1)];
        let q2 = self.exceedances[k2.min(self.exceedances.len() - 1)];
        let q3 = self.exceedances[k3.min(self.exceedances.len() - 1)];

        self.xi = (q2 - q1).ln() / 2.0f64.ln() - (q3 - q2).ln() / 2.0f64.ln();
        self.xi = self.xi.clamp(-0.5, 0.5);

        // Оценка σ
        self.sigma = (self.xi * (q2 - q1)) / (2.0f64.powf(self.xi) - 1.0);
        self.sigma = self.sigma.max(0.1);
    }

    /// Экстраполяция на P99
    pub fn quantile(&self, p: f64) -> f64 {
        if self.exceedances.is_empty() {
            return 0.0;
        }

        let _p_exceed = self.exceedances.len() as f64 / 100.0; // ~1% превышений

        if self.xi.abs() < 1e-6 {
            self.threshold + self.sigma * (1.0 - p).ln()
        } else {
            self.threshold + self.sigma / self.xi * ((1.0 - p).powf(-self.xi) - 1.0)
        }
    }
}

pub struct AdaptiveBatcher {
    // Конфигурация
    pub config: AdaptiveBatcherConfig,

    // Текущее состояние
    pub current_batch_size: RwLock<usize>,
    pub current_workers: RwLock<usize>,

    // Математические модели
    pub model_params: RwLock<BatchModelParams>,
    pub kalman: RwLock<KalmanFilter>,
    pub markov: RwLock<MarkovChain2nd>,
    pub wavelet: RwLock<WaveletTransformer>,
    pub pid: RwLock<PIDController>,
    pub gibbs: RwLock<GibbsSampler>,
    pub gps: RwLock<GPScheduler>,
    pub mpc: RwLock<ModelPredictiveController>,
    pub evt: RwLock<ExtremeValueTheory>,

    // История для обучения
    pub measurements: RwLock<Vec<(usize, f64)>>, // (batch_size, processing_time)
    pub latencies: RwLock<Vec<f64>>,
    pub lambdas: RwLock<Vec<f64>>,

    // Метрики
    pub metrics: Arc<DashMap<String, f64>>,
}

impl AdaptiveBatcher {
    pub fn new(config: AdaptiveBatcherConfig) -> Self {
        info!("🚀 Initializing Mathematical AdaptiveBatcher v2.0");

        let model_params = BatchModelParams::default();

        // ИСПРАВЛЕНО: начальная нагрузка должна быть 0, а не 100
        let kalman = KalmanFilter::new(0.0);  // Было 100.0, стало 0.0
        let markov = MarkovChain2nd::new(10);  // 10 уровней квантования
        let wavelet = WaveletTransformer::new(4, 1000);

        // ПИД с автонастройкой (Kp_crit = 0.5, T_crit = 10)
        let pid = PIDController::auto_tune(0.5, 10.0);

        let gibbs = GibbsSampler::new();
        let gps = GPScheduler::new(100000.0); // 100k ops/s
        let mpc = ModelPredictiveController::new(10, model_params);
        let evt = ExtremeValueTheory::new();

        Self {
            config: config.clone(),
            current_batch_size: RwLock::new(config.initial_batch_size),
            current_workers: RwLock::new(4),
            model_params: RwLock::new(model_params),
            kalman: RwLock::new(kalman),
            markov: RwLock::new(markov),
            wavelet: RwLock::new(wavelet),
            pid: RwLock::new(pid),
            gibbs: RwLock::new(gibbs),
            gps: RwLock::new(gps),
            mpc: RwLock::new(mpc),
            evt: RwLock::new(evt),
            measurements: RwLock::new(Vec::with_capacity(1000)),
            latencies: RwLock::new(Vec::with_capacity(1000)),
            lambdas: RwLock::new(Vec::with_capacity(1000)),
            metrics: Arc::new(DashMap::new()),
        }
    }

    pub async fn update_model(&self) {
        let measurements = self.measurements.read().await;

        if measurements.len() >= 50 {
            let mut gibbs = self.gibbs.write().await;

            // Запуск цепи Гиббса
            gibbs.run_chain(&measurements, 2000, 1000);

            // Усреднение
            let (alpha, beta, gamma, delta, b_opt) = gibbs.average();

            // Обновление модели
            let mut params = self.model_params.write().await;
            params.alpha = alpha;
            params.beta = beta;
            params.gamma = gamma;
            params.delta = delta;
            params.b_opt = b_opt;
            params.confidence = (gibbs.alpha_samples.len() as f64 / 1000.0).min(1.0);

            debug!("📊 Bayesian parameter update:");
            debug!("  α = {:.4} ms", alpha);
            debug!("  β = {:.4} ms", beta);
            debug!("  γ = {:.6} ms/B", gamma);
            debug!("  δ = {:.8} ms/B²", delta);
            debug!("  B* = {:.1}", b_opt);
            debug!("  confidence = {:.1}%", params.confidence * 100.0);

            self.record_metric("model.alpha", alpha);
            self.record_metric("model.beta", beta);
            self.record_metric("model.gamma", gamma);
            self.record_metric("model.delta", delta);
            self.record_metric("model.b_opt", b_opt);
            self.record_metric("model.confidence", params.confidence);
        }
    }

    pub async fn estimate_lambda(&self, measured_throughput: f64) -> f64 {
        let mut kalman = self.kalman.write().await;

        if self.lambdas.read().await.is_empty() {
            kalman.state = 0.0;
            kalman.covariance = 1.0;
            return 0.0;
        }

        kalman.predict();
        let lambda = kalman.update(measured_throughput.max(0.1));

        self.record_metric("lambda.estimated", lambda);
        self.record_metric("lambda.covariance", kalman.covariance);

        lambda
    }

    pub async fn predict_load(&self, horizon: usize) -> Vec<f64> {
        let wavelet = self.wavelet.write().await;

        // Вейвлет-предсказание
        let wavelet_pred = wavelet.predict(horizon);

        // Марковское предсказание
        let markov = self.markov.write().await;
        let lambdas = self.lambdas.read().await;

        if lambdas.len() >= 3 {
            let max_lambda = lambdas.iter().fold(0.0_f64, |a, &b| a.max(b)).max(1000.0);
            let l_tm1 = markov.quantize(lambdas[lambdas.len() - 2], max_lambda);
            let l_t = markov.quantize(lambdas[lambdas.len() - 1], max_lambda);
            let (l_tp1, prob) = markov.predict(l_tm1, l_t);

            if prob > 0.5 {
                let markov_pred = markov.dequantize(l_tp1, max_lambda);

                // Комбинированный прогноз (взвешенное среднее)
                let mut combined = Vec::with_capacity(horizon);
                for i in 0..horizon {
                    let w = 1.0 - (i as f64 / horizon as f64) * 0.5; // Марков вес уменьшается
                    combined.push(w * markov_pred + (1.0 - w) * wavelet_pred[i]);
                }
                return combined;
            }
        }

        wavelet_pred
    }

    pub async fn compute_qos_waiting_times(&self, lambda: f64) -> [f64; 5] {
        let mut gps = self.gps.write().await;
        gps.recompute_shares();

        let current_b = *self.current_batch_size.read().await;
        let b_f64 = current_b as f64;
        let service_rate = 1000.0; // ops/ms

        let mut waiting_times = [0.0; 5];
        for i in 0..5 {
            waiting_times[i] = gps.waiting_time(i, lambda, b_f64, service_rate);

            self.record_metric(&format!("qos.waiting_time.{}", i), waiting_times[i]);
        }

        waiting_times
    }

    pub async fn optimal_batch_size(&self, lambda: f64, waiting_time: f64) -> usize {
        let params = self.model_params.read().await;

        // L(B) = B/λ + α/B + β + γ·B + δ·(B-B_opt)² + W
        // dL/dB = 1/λ - α/B² + γ + 2δ·(B - B_opt) = 0

        let a = 2.0 * params.delta;
        let b = params.gamma - 2.0 * params.delta * params.b_opt + 1.0 / lambda.max(0.1);
        let c = 0.0;
        let d = -params.alpha;

        let roots = solve_cubic(a, b, c, d);

        let mut optimal = params.b_opt as usize;
        let mut min_latency = f64::INFINITY;

        for &root in &roots {
            if root >= self.config.min_batch_size as f64 &&
                root <= self.config.max_batch_size as f64 {

                let b = root as usize;
                let latency = b as f64 / lambda +
                    params.processing_time(b) +
                    waiting_time;

                if latency < min_latency {
                    min_latency = latency;
                    optimal = b;
                }
            }
        }

        // Проверка границ
        optimal = optimal.clamp(self.config.min_batch_size, self.config.max_batch_size);

        self.record_metric("batch.optimal", optimal as f64);
        self.record_metric("batch.min_latency", min_latency);

        optimal
    }

    pub async fn pid_correction(&self, target_latency: f64, measured_latency: f64) -> f64 {
        let mut pid = self.pid.write().await;
        let error = target_latency - measured_latency;
        let correction = pid.compute(error);

        self.record_metric("pid.error", error);
        self.record_metric("pid.correction", correction);
        self.record_metric("pid.integral", pid.integral);

        correction
    }

    pub async fn mpc_optimize(&self, lambda_pred: Vec<f64>) -> (usize, usize) {
        let current_b = *self.current_batch_size.read().await;
        let current_m = *self.current_workers.read().await;

        let mut mpc = self.mpc.write().await;
        mpc.set_lambda_predictions(lambda_pred);

        let (optimal_b, optimal_m) = mpc.solve(current_b, current_m);

        self.record_metric("mpc.optimal_b", optimal_b as f64);
        self.record_metric("mpc.optimal_m", optimal_m as f64);

        (optimal_b, optimal_m)
    }

    pub async fn update_evt(&self) {
        let latencies = self.latencies.read().await;

        if latencies.len() >= 100 {
            let mut evt = self.evt.write().await;
            evt.fit(&latencies, 0.95);

            let p99 = evt.quantile(0.99);
            let p999 = evt.quantile(0.999);

            self.record_metric("latency.p99", p99);
            self.record_metric("latency.p999", p999);

            debug!("📈 EVT estimates: P99 = {:.2} ms, P999 = {:.2} ms", p99, p999);
        }
    }

    pub async fn compute_batch_size(&self) -> usize {
        let now = Instant::now();

        // 1. Получаем измерения
        let measured_throughput = self.get_measured_throughput().await;
        let measured_latency = self.get_measured_latency().await;
        let target_latency = self.config.target_latency.as_millis() as f64;

        // 2. Оценка λ через Калман
        let lambda = self.estimate_lambda(measured_throughput).await;

        // 3. Предсказание нагрузки
        let lambda_pred = self.predict_load(10).await;

        // 4. QoS расчёт
        let waiting_times = self.compute_qos_waiting_times(lambda).await;
        let waiting_time = waiting_times[2]; // Normal priority

        // 5. Оптимальный B из кубического уравнения
        let b_optimal = self.optimal_batch_size(lambda, waiting_time).await;

        // 6. ПИД-коррекция
        let pid_correction = self.pid_correction(target_latency, measured_latency).await;

        // 7. MPC оптимизация
        let (b_mpc, m_mpc) = self.mpc_optimize(lambda_pred).await;

        // 8. Комбинированное решение
        let b_final = ((b_optimal as f64 + pid_correction + b_mpc as f64) / 3.0)
            .round() as usize;

        let b_final = b_final.clamp(self.config.min_batch_size, self.config.max_batch_size);

        // 9. Обновление количества воркеров
        *self.current_workers.write().await = m_mpc;

        let elapsed = now.elapsed().as_micros() as f64;
        self.record_metric("batch.compute_time_us", elapsed);
        self.record_metric("batch.final_size", b_final as f64);
        self.record_metric("workers.current", m_mpc as f64);

        b_final
    }

    pub async fn record_batch_execution(
        &self,
        batch_size: usize,
        processing_time: Duration,
        success_rate: f64,
        queue_depth: usize,
    ) {
        let now = Instant::now();
        let processing_ms = processing_time.as_millis() as f64;
        let throughput = batch_size as f64 / (processing_time.as_secs_f64() + 0.001);

        // Сохраняем измерение
        {
            let mut measurements = self.measurements.write().await;
            measurements.push((batch_size, processing_ms));
            if measurements.len() > 1000 {
                measurements.remove(0);
            }
        }

        {
            let mut latencies = self.latencies.write().await;
            latencies.push(processing_ms);
            if latencies.len() > 1000 {
                latencies.remove(0);
            }
        }

        {
            let mut lambdas = self.lambdas.write().await;
            lambdas.push(throughput);
            if lambdas.len() > 1000 {
                lambdas.remove(0);
            }
        }

        // Обновляем вейвлет
        {
            let mut wavelet = self.wavelet.write().await;
            wavelet.add_observation(throughput);
        }

        // Обновляем цепь Маркова
        {
            let mut markov = self.markov.write().await;
            let lambdas = self.lambdas.read().await;
            if lambdas.len() >= 3 {
                let max_lambda = lambdas.iter().fold(0.0_f64, |a, &b| a.max(b)).max(1000.0);
                let l_tm1 = markov.quantize(lambdas[lambdas.len() - 3], max_lambda);
                let l_t = markov.quantize(lambdas[lambdas.len() - 2], max_lambda);
                let l_tp1 = markov.quantize(lambdas[lambdas.len() - 1], max_lambda);
                markov.update(l_tm1, l_t, l_tp1);
            }
        }

        // Периодическое обновление модели (каждые 60 сек)
        static LAST_UPDATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now_sec = now.elapsed().as_secs();
        let last = LAST_UPDATE.load(std::sync::atomic::Ordering::Relaxed);

        if now_sec - last >= 60 {
            LAST_UPDATE.store(now_sec, std::sync::atomic::Ordering::Relaxed);
            self.update_model().await;
            self.update_evt().await;
        }

        // Запись метрик
        self.record_metric("batch.size", batch_size as f64);
        self.record_metric("batch.processing_time_ms", processing_ms);
        self.record_metric("batch.success_rate", success_rate);
        self.record_metric("batch.throughput", throughput);
        self.record_metric("queue.depth", queue_depth as f64);
    }

    /// Вспомогательные методы
    async fn get_measured_throughput(&self) -> f64 {
        self.metrics
            .get("batch.throughput")
            .map(|m| *m.value())
            .unwrap_or(0.0)  // Было 100.0, стало 0.0
    }

    async fn get_measured_latency(&self) -> f64 {
        self.metrics
            .get("batch.processing_time_ms")
            .map(|m| *m.value())
            .unwrap_or(0.0)  // Было 50.0, стало 0.0
    }

    pub fn record_metric(&self, key: &str, value: f64) {
        self.metrics.insert(key.to_string(), value);
    }

    pub async fn get_batch_size(&self) -> usize {
        *self.current_batch_size.read().await
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveBatcherConfig {
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub initial_batch_size: usize,
    pub target_latency: Duration,
    pub adaptation_interval: Duration,
}

impl Default for AdaptiveBatcherConfig {
    fn default() -> Self {
        Self {
            min_batch_size: 32,
            max_batch_size: 1024,
            initial_batch_size: 256,
            target_latency: Duration::from_millis(50),
            adaptation_interval: Duration::from_secs(1),
        }
    }
}